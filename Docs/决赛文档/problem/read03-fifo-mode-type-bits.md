# read03: FIFO 创建后 stat 模式类型位错误

## 背景

LTP `read03` 测试 FIFO 的阻塞读写行为。测试流程为：
1. `mknod(path, S_IFIFO | 0666, 0)` 创建命名管道
2. `stat(path, &st)` 获取文件元数据
3. 断言 `S_ISFIFO(st.st_mode)` — 检查 `st_mode` 文件类型位为 FIFO

## 现象

`log.ans` 中 musl 和 glibc 的 `read03` 均输出：

```
read03.c:38: TBROK: Mode does not indicate fifo file
```

mknodat 返回成功，但 `stat` 返回的 `st_mode` 类型高位为普通文件 (0x8000) 而非 FIFO (0x1000)。

## 分析

### mknodat 创建流程

`sys_mknodat` (`os/src/syscall/fs/ctl/directory.rs:62`) 创建 FIFO 分两步：

1. `root.create(&abs_path, InodeType::Fifo)` — 在 ext4 上分配新 inode
2. `inode.fmode_set((type_mode | perm) as u32)` — 设置 inode mode

### 根因

**步骤 1** 中，`Ext4Inode::create()` 调用 `nfile.file_open(path, O_RDWR | O_CREAT | O_TRUNC)`，
后者经 `ext4_fopen` → `ext4_generic_open(..., file_expect=true)` 硬编码
`filetype = EXT4_DE_REG_FILE`。新 inode 被 `ext4_fs_alloc_inode` 分配时，
mode 高位被初始化为 `ext4_fs_correspond_inode_mode(EXT4_DE_REG_FILE)` =
`EXT4_INODE_MODE_FILE` (0x8000)，而非 FIFO 应有的 `EXT4_INODE_MODE_FIFO` (0x1000)。

**步骤 2** 中，`fmode_set` 调用 lwext4 的 `ext4_mode_set`，该 C 函数行为为：
```c
orig_mode = ext4_inode_get_mode(...);  // 读出现有 mode（含错误类型位 0x8000）
orig_mode &= ~0xFFF;                    // 仅清除低 12 位权限
orig_mode |= mode & 0xFFF;             // 写入新权限，高位类型位不变
```
`ext4_mode_set` 不修改 mode 高位 (bit 12–15)，只更新低 12 位权限。因此
`fmode_set(0x1000 | 0666)` 调用后，磁盘 inode 的 `st_mode` 仍为
`0x8000 | 0666`（普通文件 + 权限），而非 `0x1000 | 0666`（FIFO + 权限）。

### stat 路径

`sys_fstatat` / `sys_statx` 通过 `open(path)` → `Ext4Inode::fstat()` 读取
lwext4 `ext4_inode_get_mode`，得到错误的类型高位。

虽有 `FsIndex::insert_special_node_type` 在 mknodat 时记录了正确的
`InodeType::Fifo`，但 `Ext4Inode::fstat()` 未使用该信息修正 `st_mode`。

## 修复

### 主要修复

`os/src/fs/ext4_lw/inode.rs` — `Ext4Inode::fstat()` 末尾：
从 `FsIndex::special_node_type()` 查询 mknodat 记录的正确 `InodeType`，
若存在则用其 `mode_bits()` 替换 `kstat.st_mode` 的高 4 位类型位。

### 防御性修复

`crates/lwext4_rust/c/lwext4/src/ext4.c` — `ext4_mode_set()`：
在修改低 12 位权限前，增加对输入 mode 高位类型位的检查与写入。
（当前因预编译 `liblwext4-loongarch64.a` 权限问题未编译进二进制，但代码正确。）

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/fs/ext4_lw/inode.rs` | 导入 `FsIndex`，`fstat()` 末尾用 `special_node_type` 修正 mode 类型位 |
| `crates/lwext4_rust/c/lwext4/src/ext4.c` | `ext4_mode_set` 在 mode 高位非零时一并写入 |

## 验证

- LoongArch64 `make` + `make run`：musl/glibc `read03` 均输出 `TPASS` 而非 `TBROK`
- 未运行 RISC-V（默认架构为 LoongArch64，RISC-V 预编译 `.a` 未受影响）
