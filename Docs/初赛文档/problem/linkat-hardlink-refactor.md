# sys_linkat 硬链接实现与 lwext4 重构

## 背景

LTP `linkat01`/`linkat02` 测例需要 `linkat(2)` 系统调用创建硬链接。此前内核 `sys_linkat` 仅有骨架代码，依赖已删除的 `OsDirent` 结构体，无法编译通过。实现 linkat 的同时需要重构 `lwext4_rust` 中的目录项处理。

## 系统架构

`sys_linkat` 的工作分布在**四层**，实际文件的存储操作在 `lwext4_rust` 中：

```c
用户态                        syscall 层                     VFS 层                    ext4 适配层              lwext4_rust
───────                      ──────────                    ───────                    ──────────              ───────────
linkat(2)  →  sys_linkat()  →  Inode::hard_link()  →  Ext4Inode::hard_link()  →  Ext4File::file_hardlink()
              os/src/syscall/    os/src/fs/vfs.rs         os/src/fs/ext4_lw/          lwext4_rust/src/file.rs
              fs/ctl.rs                                    inode.rs
                                                                                          │
                                                                                          ▼
                                                                                    ext4_flink()
                                                                                    FFI → C 库 lwext4
```

### 各层职责

| 层 | 文件 | 职责 |
|----|------|------|
| **syscall** | `os/src/syscall/fs/ctl.rs` | 参数校验（空路径→ENOENT、目录→EPERM、目标已存在→EEXIST），路径解析，AT_EMPTY_PATH 处理 |
| **VFS trait** | `os/src/fs/vfs.rs` | 定义 `fn hard_link(&self, old: &str, new: &str) -> SyscallRet` 接口 |
| **ext4 适配** | `os/src/fs/ext4_lw/inode.rs` | 调用 `lwext4_rust` 的 `file_hardlink`，错误码转换 |
| **lwext4_rust** | `lwext4_rust/src/file.rs` | 通过 FFI 调用 C 库 `ext4_flink()`，完成磁盘上的目录项创建和 inode 引用计数更新 |

**关键结论：文件系统级别的硬链接创建完全在 `lwext4_rust` 的 `file_hardlink()` 中完成，内核代码只做校验和路由。**

## 实现要点

### 1. syscall 层：`sys_linkat`

`os/src/syscall/fs/ctl.rs`

- **常规路径**（path 非空）：
  1. 解析 `oldpath` / `newpath` 为绝对路径
  2. 打开旧文件 → 检查不是目录（`EPERM`）
  3. 检查新路径不存在（`EEXIST`）
  4. 调用 `inode.hard_link(old_path, new_path)`
  5. 将新路径插入 `FsIndex` 缓存（与旧 inode 共享）

- **AT_EMPTY_PATH 路径**（`flags & AT_EMPTY_PATH`，oldpath 为空）：
  1. 通过 `oldfd` 获取已打开文件的 inode
  2. 检查 newpath 非空（`ENOENT`）
  3. 检查新路径不存在（`EEXIST`）
  4. 直接在已有 inode 上调用 `hard_link`
  5. 更新 `FsIndex` 缓存

### 2. VFS 接口：Inode trait 新增

`os/src/fs/vfs.rs`：新增 trait 方法：
```rust
/// 创建硬链接
fn hard_link(&self, _old_path: &str, _new_path: &str) -> SyscallRet {
    unimplemented!("Inode::hard_link")
}
```

### 3. ext4 适配层：Ext4Inode

`os/src/fs/ext4_lw/inode.rs`：委托给 lwext4_rust：
```rust
fn hard_link(&self, old_path: &str, new_path: &str) -> SyscallRet {
    let file = &mut self.inner.get_unchecked_mut().f;
    file.file_hardlink(old_path, new_path)
        .map_or(Err(SysErrNo::ENOENT), |_| Ok(0))
}
```

### 4. lwext4_rust 层：Ext4File

`lwext4_rust/src/file.rs`：新增 `file_hardlink()`：
```rust
pub fn file_hardlink(&mut self, path: &str, hardlink_path: &str) -> Result<usize, i32> {
    let c_path = CString::new(path).unwrap().into_raw();
    let c_hardlink_path = CString::new(hardlink_path).unwrap().into_raw();
    let r = unsafe { ext4_flink(c_path, c_hardlink_path) };
    // cleanup raw pointers
    drop(CString::from_raw(c_path));
    drop(CString::from_raw(c_hardlink_path));
    if r != EOK as i32 { return Err(r); }
    Ok(EOK as usize)
}
```

`ext4_flink` 是 lwext4 C 库的函数，负责：
- 在新路径的父目录中创建目录项，指向旧 inode
- 递增旧 inode 的硬链接计数
- 写回 superblock（如有必要）

### 5. 附属重构：消除 `OsDirent` 重复定义

此前 `OsDirent` 在 `lwext4_rust/src/file.rs` 和 `os/src/fs/ext4_lw/dirent.rs` 中各有一份定义。重构统一到 `lwext4_rust` 中：

- 删除 `os/src/fs/ext4_lw/dirent.rs` 和 `mod.rs` 中的引用
- 在 `lwext4_rust` 的 `OsDirent` 上添加 `len()` / `off()` / `as_bytes()` 方法
- `Ext4Inode::read_dentry` 直接使用 `lwext4_rust::file::OsDirent`

## 硬链接语义

硬链接与符号链接的关键区别：

| | 硬链接 | 符号链接 |
|--|--------|----------|
| 实现 | 目录项指向同一 inode | 文件内容存储目标路径 |
| inode 引用计数 | +1 | 不变 |
| 跨文件系统 | 不支持 | 支持 |
| 目录硬链接 | Linux 禁止（EPERM） | 可创建 |
| 源文件删除后 | 链接仍有效 | 悬空链接 |

## 涉及文件

| 文件 | 修改内容 |
|------|----------|
| `os/src/syscall/fs/ctl.rs` | 重写 `sys_linkat`：AT_EMPTY_PATH 支持、参数校验、目录检查、FsIndex 同步 |
| `os/src/fs/vfs.rs` | Inode trait 新增 `hard_link` 方法 |
| `os/src/fs/ext4_lw/inode.rs` | 实现 `hard_link`，`read_dentry` 改用 `lwext4_rust` 的 `OsDirent` |
| `os/src/fs/ext4_lw/dirent.rs` | **删除**（OsDirent 统一到 lwext4_rust） |
| `os/src/fs/ext4_lw/mod.rs` | 移除 dirent 模块引用 |
| `lwext4_rust/src/file.rs` | 新增 `file_hardlink()`、`OsDirent` 新增 `len()`/`off()`/`as_bytes()` |

## 验证

RISC-V `make run`，运行 `linkat01` / `linkat02` LTP 测例。

## 其他硬链接相关 syscall

同一模式可扩展实现：
- `sys_link` — 不带 AT_EMPTY_PATH 的简化版本，底层均委托给 `hard_link()`
