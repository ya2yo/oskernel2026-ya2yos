# waitpid13: WUNTRACED stopped child

## 背景

LTP `waitpid13` 验证 `waitpid()` 在 `WUNTRACED` 下能等待 stopped 子进程。测试会创建两组子进程，子进程通过 `kill(getpid(), SIGSTOP)` 停止；父进程分别使用 `waitpid(0, ..., WUNTRACED)` 和 `waitpid(-pgid, ..., WUNTRACED)` 回收 stop 事件，检查 `WIFSTOPPED(status)` 和 `WSTOPSIG(status) == SIGSTOP` 后发送 `SIGCONT`，最终再等待子进程正常退出。

## 现象

新的 `log.ans` 没有 `TFAIL/TBROK`，而是 QEMU 被外部 timeout 结束。尾部显示父进程阻塞在：

```text
sys_waitpid <= pid: 0, options: WUNTRACED | WSTOPPED
sys_waitpid: my children ... [(5, 17, false), ..., (12, 17, false)]
```

随后多个子进程已经处理 `SIGSTOP`：

```text
handle_signal: stop, signo=19
stop_current_and_run_next()
```

但父进程没有从 `waitpid(..., WUNTRACED)` 返回 stopped child，最终卡死到 QEMU timeout。

## 分析

此前 `waitid07` 已为 stopped 事件增加 `ProcessMeta::stopped_signal`，`waitid(WSTOPPED)` 会读取该字段并返回 `CLD_STOPPED`。但 `sys_waitpid()` 只检查 `child.all_tasks_exited()`，完全没有处理 `WUNTRACED/WSTOPPED`，所以即使子进程已经进入 `TaskStatus::Stopped` 并记录了 `stopped_signal`，父进程仍然认为没有可返回事件，继续阻塞。

`waitpid_common.h` 中 `reap_children()` 会对 stopped status 调用：

```c
WIFSTOPPED(status)
WSTOPSIG(status)
```

因此 `waitpid()` 需要写入传统 wait status 格式：低 8 位为 `0x7f`，高位保存 stop signal，即 `(SIGSTOP << 8) | 0x7f`。

## 根因

stopped child 事件只接入了 `waitid(WSTOPPED)`，没有接入 `waitpid(..., WUNTRACED)`；`sys_waitpid()` 在没有 exited child 时会阻塞，导致 `waitpid13` 卡死。

## 修复

在 `sys_waitpid()` 中：

- 当 options 包含 `WUNTRACED` 或 `WSTOPPED` 时，优先扫描匹配 child 的 `stopped_signal`。
- 找到 stopped child 后，向用户 `wstatus` 写入 `((signo as i32) << 8) | 0x7f`。
- 消费该 child 的 `stopped_signal`，返回 stopped child pid。
- 没有 stopped event 时继续原有 exited / `WNOHANG` / 阻塞路径。

## 涉及文件

- `os/src/syscall/task/wait.rs`

## 验证

已执行：

```text
make
timeout 90s make run
```

结果：

```text
waitpid_common.h:129: TINFO: Sending SIGCONT to 9
waitpid_common.h:129: TINFO: Sending SIGCONT to 10
waitpid_common.h:129: TINFO: Sending SIGCONT to 11
waitpid_common.h:129: TINFO: Sending SIGCONT to 12
waitpid_common.h:129: TINFO: Sending SIGCONT to 5
waitpid_common.h:129: TINFO: Sending SIGCONT to 6
waitpid_common.h:129: TINFO: Sending SIGCONT to 7
waitpid_common.h:129: TINFO: Sending SIGCONT to 8
waitpid13.c:70: TPASS: Test PASSED

Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```
