# LTP waitpid04 非法 options 错误码修复

## 背景

最新 `log.ans` 中 musl 和 glibc 的 `waitpid04` 均有一项失败：`waitpid(-1, NULL, 0xffffffff)` 期望返回 `EINVAL`，实际返回 `ECHILD`。

## 分析

`sys_waitpid()` 使用 `WaitOption::from_bits_truncate()` 解析 options。该函数会静默丢弃未知位，因此 `0xffffffff` 被截断为若干已知等待选项，随后才进入子进程筛选。由于当前进程没有符合条件的 child，函数返回了 `ECHILD`，掩盖了调用参数本身非法这一事实。

`sys_waitid()` 已经使用严格的 `WaitOption::from_bits()` 校验，waitpid/wait4 应保持相同的未知位处理语义。

## 根因

waitpid 的 options 使用截断解析而不是严格解析，导致非法 option 位未返回 `EINVAL`。

## 修复

将 `sys_waitpid()` 的 options 解析改为 `WaitOption::from_bits(options).ok_or(SysErrNo::EINVAL)?`，在 pid 处理和 child 查找前拒绝未知位，避免返回误导性的 `ECHILD`。

## 涉及文件

- `os/src/syscall/task/wait.rs`

## 验证

`cargo fmt --manifest-path os/Cargo.toml -- --check` 通过；根目录 `make` 的 RISC-V 和 LoongArch64 构建均通过。`timeout 120s make run` 中 RISC-V musl/glibc 的 `waitpid04` 均为 `passed 4 failed 0 broken 0 skipped 0 warnings 0`，并正常 `shutdown!`。
