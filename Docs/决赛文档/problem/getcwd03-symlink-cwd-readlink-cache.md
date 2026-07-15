# LTP getcwd03 符号链接 cwd 与 readlink 语义修复

## 背景

LTP `getcwd03` 创建目录 `getcwd1.<pid>` 与指向它的相对符号链接
`getcwd2.<pid>`。测试分别通过真实目录和该符号链接进入同一目录，要求两次
`getcwd()` 返回相同的规范路径；随后以 `readlink()` 读取符号链接内容。

## 现象

原始 LoongArch64 `log.ans` 中，musl 与 glibc 都在 `getcwd03.c:60` 失败：

```text
getcwd() got mismatched working directories
(/tmp/.../getcwd1.<pid>, /tmp/.../getcwd2.<pid>)
```

第一处修复使两次工作目录一致后，测试继续在 `getcwd03.c:67` 以
`readlink(...)=EINVAL` 报 `TBROK`。此前失败后的 cleanup 还曾出现
`unlink(.../getcwd2.<pid>) = EISDIR`，说明符号链接的缓存查找也没有保留末级链接本身。

## 分析

`sys_chdir()` 会调用 `open()`，而 ext4 的路径查找会跟随末级符号链接并得到真实目录
inode；但成功后原实现把输入字符串 `abs_path` 写回 `FSInfo.cwd`。`sys_getcwd()` 只复制
该字符串，所以通过 `getcwd2` 进入后错误返回链接别名，而不是目标目录路径。

`sys_readlinkat()` 也以普通 `open()` 查找末级路径，导致符号链接被解析为目录，类型检查
返回 `EINVAL`。即使调用者使用内部 `O_UNLINK` 标志，`open()` 仍可从 `FsIndex` 或 dentry
positive cache 命中此前以链接名缓存的目标目录 inode，破坏 `readlinkat()` 和 `unlinkat()`
需要操作链接自身的语义。

## 根因

路径解析得到的 inode 与进程保存的 cwd 字符串不一致；同时 VFS 缓存没有把“保留末级
symlink”的查找从已跟随链接的缓存结果中隔离出来。

## 修复

- `sys_chdir()` 在确认目标为目录后，保存 `osfile.inode.path()`，即解析后的目录路径；
- `sys_readlinkat()` 使用内部 `OpenFlags::O_UNLINK` 查找末级路径，以读取链接对象而非目标；
- `open()` 对 `O_UNLINK` 与 `O_NOFOLLOW` 一样绕过 `FsIndex` 和 dentry cache 的命中及回填，
  让底层路径查找保留末级符号链接。

## 涉及文件

- `os/src/syscall/fs/path.rs`
- `os/src/fs/kernel_fs_ops/open.rs`

## 验证

- `cargo fmt --manifest-path os/Cargo.toml -- --check` 通过；
- `make TARGET_ARCH=loongarch64` 通过，根 Makefile 实际完成 RISC-V 与 LoongArch64 构建，
  仅有既有 vendored `smoltcp` warning；
- `timeout 120s make TARGET_ARCH=loongarch64 run > log.ans 2>&1`：musl/glibc `getcwd03`
  均为 `passed 1 failed 0 broken 0 warnings 0`，并正常 `shutdown!`；
- `timeout 120s make TARGET_ARCH=riscv64 run > /tmp/getcwd03-riscv64.log 2>&1`：musl/glibc
  同样均为 `passed 1 failed 0 broken 0 warnings 0`，并正常 `shutdown!`。
