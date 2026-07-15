# signal 默认 disposition ABI 与 SIGSTOP 卡死修复

## 背景

提交 `b31376b77242ce3765b9452cafb9ed2c6b758cf2` 修复了 LTP `signal03`：
`SIGTSTP`、`SIGTTIN` 和 `SIGTTOU` 虽然默认动作为 stop，但用户可以将它们显式设为
`SIG_IGN`，此时不能停止进程。该结论与 Linux 语义一致，应当保留。

随后单跑 LTP `waitid07` 时，子进程对自身发送不可忽略的 `SIGSTOP`，父进程等待
`waitid(P_PID, ..., WSTOPPED | WNOWAIT)`，却在 checkpoint 等待 10 秒后超时。

## 现象

原始 `log.ans` 的有效失败信号为：

```text
waitid07.c:26: TBROK: tst_checkpoint_wait(0, 10000) failed: ETIMEDOUT (110)
```

`waitid07` 的子进程必须在 `SIGSTOP` 后进入 stopped 状态，父进程才能取得
`CLD_STOPPED`，发送 `SIGCONT` 并解除子进程 checkpoint。子进程未停止会使父子双方永久等待。

## 分析

旧 `SigAction::new()` 用 `sa_handler == SIG_IGN` 表示默认的 ignore、continue 和 stop
动作，用内核 `exit_current_and_run_next` 函数地址表示默认 terminate 动作。这与 Linux
`sigaction` ABI 不符：用户态可见的默认 disposition 必须是 `SIG_DFL (0)`，而 `SIG_IGN (1)`
只能表示显式忽略。

因此，`b31376b` 将 pending 分发改为“看到 `SIG_IGN` 就忽略”后，内部默认的 `SIGSTOP`
也被误认为用户显式忽略，未进入 stopped 状态。此前以 `default_op != SigOp::Stop` 作为
例外的版本虽然会停止 `SIGSTOP`，却同样错误地停止了可显式忽略的 `SIGTSTP`、`SIGTTIN`
和 `SIGTTOU`。

## 根因

内核 action 表混用了两种不同概念：

- Linux 用户 ABI 中的 `sa_handler` 值；
- 内核依据 signal number 决定的默认 disposition。

这使显式 `SIG_IGN` 与默认 stop 在表中无法区分，也使 `rt_sigaction(..., oldact)` 可能向
用户态返回 `SIG_IGN` 或内核函数地址，而不是 Linux 规定的 `SIG_DFL`。

## 修复

引入内部 `SigDisposition::{Default, Ignore, Handler}`，由 `KSigAction` 显式保存 action
来源，并提供 `is_ignored()`、`is_handler()` 查询。

- 默认 action 的 `SigAction.sa_handler` 统一保存为 `SIG_DFL`；具体 terminate/stop/ignore/
  continue 动作仍在 pending 分发时依据 signal number 的 `default_op()` 决定。
- 只有 `rt_sigaction()` 收到用户 `SIG_IGN` 时创建 `Ignore` action；用户 handler 创建
  `Handler` action。
- `rt_sigaction(..., SIG_DFL)`、`SA_RESETHAND` 和新建 `SigTable` 均使用统一的 default
  action；`oldact` 因此返回 Linux ABI 正确的 `SIG_DFL`。
- trap return、信号投递、`wait*`、`pselect6` 和 `ppoll` 全部改用 disposition helper，
  不再直接由 `sa_handler` 的内部编码推断信号语义。

涉及文件：

- `os/src/signal/types.rs`
- `os/src/signal/action_table.rs`
- `os/src/signal/frame.rs`
- `os/src/signal/pending.rs`
- `os/src/signal/delivery.rs`
- `os/src/syscall/signal.rs`
- `os/src/syscall/task/wait.rs`
- `os/src/syscall/io_mpx/select.rs`
- `os/src/syscall/io_mpx/poll.rs`
- `os/src/trap/mod.rs`

## 验证

执行：

```text
make TARGET_ARCH=loongarch64 log
timeout 120s make TARGET_ARCH=loongarch64 run > /tmp/waitid07-disposition-loongarch64.log 2>&1
make TARGET_ARCH=riscv64 log
timeout 120s make TARGET_ARCH=riscv64 run > /tmp/waitid07-disposition-riscv64.log 2>&1
```

结果：

- LoongArch64 与 RISC-V debug 构建均通过，只有既有 vendored `smoltcp` warning。
- 两个架构的 musl、glibc `waitid07` 均为 `passed 5 failed 0 broken 0`，并最终打印
  `shutdown!`。
- 两轮测试均确认 `waitid(WSTOPPED | WNOWAIT)` 成功，且 `si_pid`、`si_status == SIGSTOP`、
  `si_signo == SIGCHLD`、`si_code == CLD_STOPPED` 全部 `TPASS`。
