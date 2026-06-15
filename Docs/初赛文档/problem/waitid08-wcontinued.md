# waitid08: WCONTINUED 事件缺失

## 背景

LTP `waitid08` 验证 `waitid()` 对 stopped / continued 子进程事件的支持：

- 子进程先向自己发送 `SIGSTOP`
- 父进程通过 `waitid(P_PID, child, infop, WSTOPPED)` 观察 `CLD_STOPPED`
- 父进程发送 `SIGCONT`
- 父进程再通过 `waitid(P_PID, child, infop, WCONTINUED)` 观察 `CLD_CONTINUED`

## 现象

新的 `log.ans` 中，`WSTOPPED` 阶段已经全部通过：

```text
waitid08.c:33: TPASS: waitid(P_PID, pid_child, infop, WSTOPPED) passed
waitid08.c:35: TPASS: infop->si_pid == pid_child (4)
waitid08.c:36: TPASS: infop->si_status == SIGSTOP (19)
waitid08.c:38: TPASS: infop->si_code == CLD_STOPPED (5)
```

随后父进程进入 `waitid(..., WCONTINUED)`，子进程等待 checkpoint 超时：

```text
waitid08.c:42: TINFO: filter child by WCONTINUED
waitid08.c:26: TBROK: tst_checkpoint_wait(0, 10000) failed: ETIMEDOUT (110)
```

日志显示父进程在 `WaitId options: WCONTINUED` 后阻塞，而子进程继续执行到 futex checkpoint wait，最终超时。

## 分析

此前内核已经支持：

- 默认停止信号让任务进入 `TaskStatus::Stopped`
- `ProcessMeta::stopped_signal` 记录 stopped event
- `waitid(WSTOPPED)` 返回 `SIGCHLD / CLD_STOPPED / stop_signal`
- `SIGCONT` 投递到 stopped task 时将任务恢复为 Ready

但 `SIGCONT` 恢复任务后没有记录 continued event，`sys_waitid()` 也没有扫描 `WCONTINUED`。因此父进程虽然成功发送 `SIGCONT`，但后续 `waitid(WCONTINUED)` 找不到可返回事件，只能阻塞等待 `child_exit_event`，导致测试同步停住。

## 根因

当前进程元数据只保存了 stopped event，没有保存 continued event；`waitid()` 实现也只处理 `WSTOPPED` 与 `WEXITED`，缺少 `WCONTINUED -> CLD_CONTINUED` 路径。

## 修复

在 `ProcessMeta` 中新增：

```rust
continued_signal: Option<usize>
```

`send_signal_to_thread_group()` 投递 `SIGCONT` 并实际恢复 stopped task 时：

- 记录 `continued_signal = Some(SIGCONT)`
- 唤醒父进程的 `child_exit_event`
- 遍历 tasks 前先 clone weak list，避免持有子进程 meta 锁时再次写同一把锁

`sys_waitid()` 增加 `WCONTINUED` 扫描：

- 找到 `continued_signal` 时填 `SIGCHLD / CLD_CONTINUED / SIGCONT`
- 非 `WNOWAIT` 时清除 continued event
- 返回值仍遵守 waitid 语义：`infop != NULL` 时返回 0

## 涉及文件

- `os/src/task/process/process.rs`
- `os/src/signal/mod.rs`
- `os/src/syscall/task/wait.rs`

## 验证

已执行：

```text
make
timeout 90s make run
```

结果：

```text
waitid08.c:33: TPASS: waitid(P_PID, pid_child, infop, WSTOPPED) passed
waitid08.c:35: TPASS: infop->si_pid == pid_child (4)
waitid08.c:36: TPASS: infop->si_status == SIGSTOP (19)
waitid08.c:37: TPASS: infop->si_signo == SIGCHLD (17)
waitid08.c:38: TPASS: infop->si_code == CLD_STOPPED (5)
waitid08.c:45: TPASS: waitid(P_PID, pid_child, infop, WCONTINUED) passed
waitid08.c:47: TPASS: infop->si_pid == pid_child (4)
waitid08.c:48: TPASS: infop->si_status == SIGCONT (18)
waitid08.c:49: TPASS: infop->si_signo == SIGCHLD (17)
waitid08.c:50: TPASS: infop->si_code == CLD_CONTINUED (6)

Summary:
passed   10
failed   0
broken   0
skipped  0
warnings 0
```

运行末尾 `/dev/shm/ltp_waitid08_2` 的 `ENOENT` 是清理阶段噪音，测试 Summary 为 failed 0。
