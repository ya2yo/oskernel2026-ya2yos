# waitid11: SIGKILL 终止状态

## 背景

LTP `waitid11` 验证 `waitid()` 能正确识别被 `SIGKILL` 杀死的子进程。

测试流程：

- 子进程执行 `pause()` 阻塞等待信号
- 父进程 `kill(child, SIGKILL)`
- 父进程调用 `waitid(P_ALL, 0, infop, WEXITED)`
- 期望 `siginfo_t` 中 `si_status == SIGKILL` 且 `si_code == CLD_KILLED`

## 现象

`log.ans` 中失败为：

```text
waitid11.c:33: TPASS: waitid(P_ALL, 0, infop, WEXITED) passed
waitid11.c:34: TPASS: infop->si_pid == pidchild (4)
waitid11.c:35: TFAIL: infop->si_status (0) != SIGKILL (9)
waitid11.c:36: TPASS: infop->si_signo == SIGCHLD (17)
waitid11.c:37: TFAIL: infop->si_code (1) != CLD_KILLED (2)
```

父进程看到了普通退出事件 `CLD_EXITED` 和 status 0，而不是信号杀死事件。

## 分析

此前 `waitid10` 已经为默认 `CoreDump` 信号路径增加了 `ProcessMeta::termination_signal`，`waitid()` 可以据此返回 `CLD_DUMPED` 或 `CLD_KILLED`。

但 `waitid11` 的子进程阻塞在 `pause()`。`SIGKILL` 投递后，阻塞 syscall / future 路径可能先被唤醒并结束进程，不一定经过 `handle_signal()` 中记录 `termination_signal` 的默认信号处理分支。结果是进程最终退出时只有普通 exit code，父进程 `waitid()` 无法判断这是 `SIGKILL` 终止。

## 根因

终止信号原因只在 `handle_signal()` 的默认 `Terminate/CoreDump` 分支记录，覆盖不到阻塞任务被 `SIGKILL` 唤醒后直接退出的路径。

## 修复

在 `send_signal_to_thread_group()` 投递进程级信号时，如果该信号的默认动作是：

- `SigOp::Terminate`：立即记录 `termination_signal = Some((signo, false))`
- `SigOp::CoreDump`：立即记录 `termination_signal = Some((signo, true))`

这样无论后续任务是在 trap 返回路径处理信号，还是在阻塞 syscall 唤醒路径退出，父进程 `waitid()` 都能从进程元数据读到结构化终止原因。

## 涉及文件

- `os/src/signal/mod.rs`

## 验证

已执行：

```text
make
timeout 90s make run
```

结果：

```text
waitid11.c:33: TPASS: waitid(P_ALL, 0, infop, WEXITED) passed
waitid11.c:34: TPASS: infop->si_pid == pidchild (4)
waitid11.c:35: TPASS: infop->si_status == SIGKILL (9)
waitid11.c:36: TPASS: infop->si_signo == SIGCHLD (17)
waitid11.c:37: TPASS: infop->si_code == CLD_KILLED (2)

Summary:
passed   5
failed   0
broken   0
skipped  0
warnings 0
```
