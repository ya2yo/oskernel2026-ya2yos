# open10: chown 与 setgid 目录中新文件 GID 继承

## 背景

LTP `open10` 验证新建文件时 owner/group 与 setgid bit 的语义：

- `chown()` 修改目录 owner/group 后，`stat()` 应能读到新 group。
- 普通目录中新建文件应使用进程 effective gid。
- setgid 目录中新建文件应继承父目录 gid。
- 显式以 `S_ISGID` 创建文件时，测试会检查 setgid bit 是否按预期保留。

## 现象

新的 `log.ans` 显示 `open10` 在 setup 阶段失败：

```text
open10.c:42: TINFO: User nobody: uid = 1, gid = 0
open10.c:44: TINFO: Found unused GID 2: SUCCESS (0)
open10.c:87: TBROK: dir_a: Incorrect group, 0 != 2
```

`open10` 调用 `SAFE_CHOWN(DIR_A, nobody_uid, free_gid)` 后立即 `stat(DIR_A)`，期望 `st_gid == free_gid`，但内核仍返回 0。

## 分析

检查 syscall 入口后发现 `sys_fchownat()` 是伪实现：

```rust
pub fn sys_fchownat(...) -> SyscallRet {
    Ok(0)
}
```

因此用户态 `chown()`/`fchownat()` 看起来成功，但 ext4 inode 的 uid/gid 根本没有更新，`stat()` 自然仍返回原 gid 0。

继续阅读 `open10.c` 后确认，仅实现 `fchownat` 还不够。测试后续会切换到 nobody 用户，并在两个目录下创建文件：

- `dir_a` 没有 setgid bit，新文件应使用进程 effective gid。
- `dir_b` 设置了 setgid bit，新文件应继承 `dir_b` 的 gid，也就是前面 `chown()` 设置的 free gid。

原 `create_file()` 只设置 mode，没有设置新 inode 的 uid/gid，也没有实现 setgid 目录 group 继承。

## 根因

根因有两点：

1. `sys_fchownat()` 直接返回成功，没有调用底层 ext4 owner 修改接口。
2. `create_file()` 新建 inode 后只设置权限位，未设置 owner/group，也未按父目录 `S_ISGID` 继承 gid。

此外，当前工作区已有改动将错误返回类型统一为 `SysResult`，但部分 fs 文件仍引用旧的 `GeneralRet`，导致修复后构建被旧别名阻塞，需要一并做兼容调整。

## 修复

修改 `crates/lwext4_rust/src/file.rs`：

- 封装已有 `ext4_owner_set(path, uid, gid)` 为 `Ext4File::file_owner_set()`。

修改 `os/src/fs/vfs.rs` 与 `os/src/fs/ext4_lw/inode.rs`：

- 为 `Inode` 增加 `owner_set(uid, gid)`。
- `Ext4Inode::owner_set()` 调用 lwext4 owner 设置接口。

修改 `os/src/syscall/fs/ctl.rs`：

- 实现 `sys_fchownat()` 的基本语义。
- 支持 `AT_EMPTY_PATH` 与 `AT_SYMLINK_NOFOLLOW` 标志检查。
- root 可修改 inode uid/gid，`owner == -1` 或 `group == -1` 时保持原值。
- 非 root 暂按 `EPERM` 拒绝。

修改 `os/src/fs/kernel_fs_ops/open.rs`：

- `create_file()` 新建 inode 后设置 owner/group。
- 默认 uid 使用进程 effective uid，gid 使用进程 effective gid。
- 若父目录设置了 `S_ISGID`，新 inode gid 继承父目录 gid。

修改 `os/src/fs/mod.rs`、`os/src/fs/fstruct.rs`、`os/src/fs/kernel_fs_ops/initfiles.rs`：

- 将旧的 `GeneralRet` 引用调整为当前代码中已有的 `SysResult`，解除构建阻塞。

## 涉及文件

- `crates/lwext4_rust/src/file.rs`
- `os/src/fs/vfs.rs`
- `os/src/fs/ext4_lw/inode.rs`
- `os/src/fs/kernel_fs_ops/open.rs`
- `os/src/syscall/fs/ctl.rs`
- `os/src/fs/mod.rs`
- `os/src/fs/fstruct.rs`
- `os/src/fs/kernel_fs_ops/initfiles.rs`

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
git diff --check
```

结果：

- 当前默认架构为 `loongarch64`，`make` 构建通过。
- `open10` musl 轮次输出 9 项 `TPASS`，Summary 为 `passed 9 failed 0 broken 0`。
- `open10` glibc 轮次输出 9 项 `TPASS`，Summary 为 `passed 9 failed 0 broken 0`。
- `git diff --check` 无输出。
- 日志中的 `FAIL LTP CASE open10 : 10` / `RESULT GLIBC LTP SINGLE CASE open10 : 10` 是当前 initproc 包装层打印的退出码行；按项目规则以 LTP `TPASS` 和 `Summary` 为准。
- 未运行 `riscv64`。
