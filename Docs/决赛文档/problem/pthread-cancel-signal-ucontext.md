# pthread_cancel 的信号来源与 ucontext ABI

## 背景

初赛 musl libc 的静态和动态 `pthread_cancel` 测试均曾超时。musl 使用内部取消信号
和 `SA_SIGINFO` handler 修改被中断线程的返回 PC，因此内核必须同时满足信号来源和
`ucontext_t` 布局约定。

## 现象

旧日志中静态、动态测试均出现：

```text
FAIL pthread_cancel [timed out]
```

临时诊断还显示 `rt_sigreturn` 恢复了原始 PC，而不是取消 trampoline 的地址。

## 分析与根因

`tkill`/`tgkill` 发送路径复用了普通 `kill` 的 `SI_USER` (`si_code = 0`)。musl
取消 handler 会检查来源，期望 Linux 的 `SI_TKILL`。

此外，RISC-V 和 LoongArch64 的内核 `UserContext` 在可扩展 signal mask 与
`uc_mcontext` 之间少了一个 ABI padding word。musl 按标准偏移写入 PC 时，实际覆盖
了错误的寄存器槽，导致 sigreturn 继续回到取消点原地址，线程无法退出而超时。

## 修复

- 增加 `SigInfo::new_tkill()`，填充 `SI_TKILL`，并用于按线程发送信号的两个路径。
- 在两个架构的 `UserContext` 中加入 `__uc_pad: usize`，signal frame 初始化为零，
  使 `ucontext_t.uc_mcontext` 与 musl ABI 对齐。

## 涉及文件

- `os/src/signal/types.rs`
- `os/src/signal/delivery.rs`
- `os/src/signal/frame.rs`
- `os/src/arch/riscv64/qemu/context/trap_context.rs`
- `os/src/arch/loongarch64/qemu/context/trap_context.rs`

## 验证

- `make log TARGET_ARCH=riscv64` 构建通过。
- 定向 `pthread_cancel` 测试由超时变为 `Pass!`。
- `make TARGET_ARCH=riscv64` 生成的 warn 级内核运行完整 preliminary musl libc 套件时，
  静态、动态 `pthread_cancel` 和 `pthread_cond_smasher` 均为 `Pass!`，217 项测试全部通过
  并输出 `shutdown!`。
- `make log` 生成的 debug 级内核会在 futex/调度热路径输出约 2.4 万条 debug 行；同一
  500 ms 超时测试在静态、动态两处均可能因串口输出时序扰动在 phase 9 超时。这是观测
  配置造成的复测假失败，不作为 futex 语义失败结论。
- LoongArch64 共享 signal/ucontext 改动已完成编译路径适配；完整运行回归未在本轮执行。
