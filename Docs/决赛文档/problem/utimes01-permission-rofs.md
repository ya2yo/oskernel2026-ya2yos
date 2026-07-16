# LTP utimes01 权限、坏指针与只读挂载语义修复

## 背景

`log.ans` 中 musl 与 glibc 的 `utimes01` 均失败。测试通过 `utimes(2)` 覆盖非 owner 的当前时间更新、NULL pathname、非 owner 显式时间值和只读 tmpfs 挂载的行为。

## 现象

修复前：

- 非 owner 对不可写文件以 `times == NULL` 更新时间戳，期望 `EACCES`，实际成功；
- `pathname == NULL`，期望 `EFAULT`，实际成功；
- 非 owner 传入显式 timeval，期望 `EPERM`，实际成功；
- 只读挂载内的文件，期望 `EROFS`，实际成功。

musl 因第三项未返回预期错误码而继续执行恢复时间戳的路径，随后出现无关的 `TBROK EINVAL`。

## 分析

本架构的 `utimes(2)` 通过 `utimensat` syscall 进入 `sys_utimensat()`。原实现把 NULL pathname 转为空字符串，且在取得 inode 后无条件调用 `set_timestamps()`，没有检查 mount flags、effective uid/gid、inode owner 或文件写权限。

Linux 对时间戳权限有两个分支：`times == NULL` 表示使用当前时间，非 owner 可在具有文件写权限时执行；显式时间值则要求 inode owner 或特权。只读挂载必须在普通权限判断前拒绝为 `EROFS`。

## 根因

`sys_utimensat()` 缺失 utimes 兼容路径所需的用户指针、挂载只读和时间戳权限检查。

## 修复

- NULL pathname 返回 `EFAULT`；
- 目标路径被只读挂载覆盖时返回 `EROFS`；
- 非 root、非 inode owner 且 `times == NULL` 时，要求对应 owner/group/other 写权限，否则返回 `EACCES`；
- 非 root、非 inode owner 传入显式时间值时返回 `EPERM`；
- 通过检查后才调用 inode 的 `set_timestamps()`。

## 涉及文件

- `os/src/syscall/fs/ctl.rs`

## 验证

`cargo fmt --manifest-path os/Cargo.toml -- --check` 通过；根目录 `make` 的 RISC-V 和 LoongArch64 构建均通过。`timeout 120s make run` 与 `timeout 120s make TARGET_ARCH=loongarch64 run` 中，两架构的 musl/glibc `utimes01` 均为 `passed 7 failed 0 broken 0 skipped 0 warnings 0`，并正常 `shutdown!`。
