# signal03 SIG_IGN 忽略 stop 信号导致卡死

## 背景

LTP `signal03` 验证可捕获信号的 `SIG_IGN` disposition。测试对 `siglist` 中的每个信号依次执行：

1. 用 `signal(signo, sighandler)` 安装会将 `ign_handler` 置为 `1` 的 handler；
2. 用 `signal(signo, SIG_IGN)` 覆盖为忽略；
3. 调用 `kill(getpid(), signo)` 向自身发送信号；
4. 断言 `ign_handler == 0`。

信号列表包含 `SIGTSTP`、`SIGTTIN` 和 `SIGTTOU`，但不包含不可设置 disposition 的 `SIGSTOP`。Linux 允许这三个 job-control stop 信号设置为 `SIG_IGN`；被忽略后不得停止进程。

## 现象

维护者将 initproc 配置为单跑 `signal03` 后，旧 `log.ans` 在 musl 测试打印 18 个
`TPASS: ign_handler == 0 (0)` 后永久停止，没有进入 Summary 或后续 glibc 测试。第 19 个待测信号为
`SIGTSTP`，与停滞位置一致。

## 分析

`sys_rt_sigaction()` 已把 `SIG_IGN` 存入进程的 `SigTable`。trap 返回路径中，`handle_signal()` 取出 pending signal 后会根据该 action 与默认动作决定处理方式。

此前为处理 `kill12` 加入的判断仅在默认动作不是 `SigOp::Stop` 时才直接消费 `SIG_IGN`：

```rust
if sig_action.act.sa_handler == SIG_IGN && default_op != SigOp::Stop {
    return;
}
```

因此显式忽略的 `SIGTSTP` 仍会落入 `SigOp::Stop` 分支，并执行 `stop_current_and_run_next()`。本测例随后没有外部 `SIGCONT`，进程无法恢复，表现为内核运行中的永久卡死。

## 根因

实现把“默认动作是 stop”误当成“不能被忽略”。Linux 中只有 `SIGKILL` 与 `SIGSTOP` 不能设置 handler、忽略或阻塞；前者已由 `sys_rt_sigaction()` 拒绝，且 `signal03` 不测试 `SIGSTOP`。`SIGTSTP`、`SIGTTIN`、`SIGTTOU` 的默认动作为 stop，但它们的 disposition 可以是 `SIG_IGN`。

## 修复

`os/src/signal/pending.rs` 中，`handle_signal()` 在非自定义 handler 路径看到显式 `SIG_IGN` 时无条件消费并返回，不再对 `SigOp::Stop` 设置例外。`SIGKILL` 与 `SIGSTOP` 的限制继续由 `sys_rt_sigaction()` 在安装 action 时保障，因此该变更不会放宽这两个不可忽略信号的约束。

涉及文件：

- `os/src/signal/pending.rs`

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

结果：

- 默认 LoongArch64 `make` 通过，仅有既有 vendored `smoltcp` warning。
- `log.ans` 中 musl `signal03` 输出 31 个 `TPASS`，Summary 为 `passed 31 failed 0 broken 0 skipped 0 warnings 0`。
- `log.ans` 中 glibc `signal03` 输出 30 个 `TPASS`，Summary 为 `passed 30 failed 0 broken 0 skipped 0 warnings 0`。
- 两组测试均返回 initproc 并最终打印 `shutdown!`；未再发生卡死。

`FAIL LTP CASE signal03 : 0` 是 musl 测试包装器对成功退出码 `0` 的既有打印，不能作为失败依据；以 LTP Summary 为准。
