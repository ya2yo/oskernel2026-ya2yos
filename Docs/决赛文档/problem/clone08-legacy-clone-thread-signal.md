# LTP clone08 legacy clone 线程退出信号兼容

## 背景

LTP `clone08` 依次覆盖 `CLONE_PARENT`、`CLONE_CHILD_SETTID`、`CLONE_PARENT_SETTID` 和 `CLONE_THREAD | CLONE_SIGHAND | CLONE_VM | CLONE_CHILD_CLEARTID`。最后一项要求新线程与调用者共用 thread group ID，并在退出时清零 `ctid`、唤醒等待该地址的 futex。

## 现象

最新 `log.ans` 中，LoongArch64 的 musl 与 glibc 都在前三项通过后报告：

```text
clone08.c:85: TBROK: CLONE_THREAD clone() failed: EINVAL (22)
```

两轮 Summary 都是 `passed 3 failed 0 broken 1`。

## 分析

`clone08.c` 通过旧式 `clone(2)` libc wrapper 传入 `CLONE_THREAD | CLONE_SIGHAND | CLONE_VM | CLONE_CHILD_CLEARTID | SIGCHLD`。内核原先把 `CLONE_THREAD` 与低位 `CSIGNAL` 非零一律判为 `EINVAL`，混淆了 clone3 的约束与 legacy clone 行为。

Linux `clone3` 将 `exit_signal` 作为独立字段，`CLONE_THREAD` 与非零值组合会被拒绝；但旧式 `clone(2)` 的低位 `CSIGNAL` 与 `CLONE_THREAD` 组合可被接受，线程不会把该值作为父进程退出信号。当前内核线程路径本来就不会以传入的 `exit_signal` 覆盖共享 `ProcessMeta`，因此只需放宽 legacy clone 参数校验。

修复后，RISC-V 的 musl/glibc 与 LoongArch64 glibc 均通过五项断言，包含 `clone has the same thread id` 和 `futex exit on ctid change, ctid: 0`。

LoongArch64 musl 仍有独立限制。debug QEMU 日志显示其在 `running CLONE_THREAD` 后没有发出 `Clone` 或 `Clone3` syscall。只读导出镜像中的 `/musl/lib/libc.so` 并反汇编 `clone` 符号可见，该 wrapper 对 `flags & 0x290000` 非零直接返回 `EINVAL`；该掩码包含 `CLONE_THREAD`、`CLONE_SETTLS` 和 `CLONE_CHILD_CLEARTID`。因此该剩余失败发生在用户态 libc、内核入口之前，不能由 syscall handler 修复。

## 根因

内核错误地将 clone3 的 `CLONE_THREAD` exit signal 限制用于旧式 `clone(2)`，使可兼容的 LTP 调用在进入线程创建路径前返回 `EINVAL`。

## 修复

- `validate_clone_flags()` 只保留 `CLONE_THREAD` 必须同时具备 `CLONE_SIGHAND` 与 `CLONE_VM` 的 Linux 约束；
- legacy clone 不再因低位 `CSIGNAL` 非零而拒绝 `CLONE_THREAD`；
- clone3 仍通过其独立的 `exit_signal` ABI 校验拒绝该组合；
- 未修改 `CLONE_THREAD` 的退出信号处理，线程仍不会覆盖线程组已有的进程退出信号。

## 涉及文件

- `os/src/syscall/task/clone.rs`

## 验证

- `cargo fmt --manifest-path os/Cargo.toml` 通过。
- `make` 通过，完成 RISC-V 与 LoongArch64 release 构建；只有既有 vendored `smoltcp` warning。
- RISC-V `timeout 120s make run > /tmp/clone08-riscv-after-fix.log 2>&1` 通过。musl/glibc 均为 `passed 5 failed 0 broken 0 skipped 0 warnings 0`，最终 `shutdown!`。
- LoongArch64 `timeout 120s make TARGET_ARCH=loongarch64 run > /tmp/clone08-loongarch-after-fix.log 2>&1`：glibc 为 `passed 5 failed 0 broken 0 skipped 0 warnings 0`；musl 仍为 `passed 3 failed 0 broken 1`，原因是镜像内 musl wrapper 的本地拒绝，未进入内核。
- `make TARGET_ARCH=loongarch64 log` 与 debug QEMU 只用于定位，随后已恢复 release 构建。
