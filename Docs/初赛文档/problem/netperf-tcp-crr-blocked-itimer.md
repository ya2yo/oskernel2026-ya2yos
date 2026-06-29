# netperf TCP_CRR: blocked accept 的 itimer 唤醒滞后

## 背景

`netperf-musl` 前序 `setsockopt/getsockopt` 兼容问题修复后，`errno 92` 已消失，但最后一个 `TCP_CRR` 子项仍失败。失败发生在测试结束阶段，客户端等待控制连接上的服务端结果响应。

## 现象

`log.ans` 中 `TCP_CRR` 末尾出现：

```text
recv_response_timed_n: no response received. errno 9 counter 0
====== netperf TCP_CRR end: fail ======
```

详细 syscall 日志显示最后一次 request/response 数据连接已经完成：

```text
Connect ret = 0
SendTo ret = 64
Accept ret = 7
RecvFrom ret = 64
SendTo ret = 64
RecvFrom ret = 64
```

随后客户端进入控制连接 `pselect6` 等待服务端 656 字节结果，但 `pselect6` 先返回超时，服务端稍后才从阻塞的 `accept` 中被 `SIGALRM` 打断并 `SendTo ret = 656`。

## 分析

`TCP_CRR` 的服务端在测试循环结束前会阻塞在下一次 `accept` 上，等待 `ITIMER_REAL` 到期后由 `SIGALRM` 打断，然后汇总结果并通过控制连接回传。Ya2yOS 的 `TcpSocket::accept()` 走 `block_on(interruptible(...))`，阻塞时会把当前任务置为 `TaskStatus::Blocked`，并依赖 `TaskControlBlock::check_timer()` 在 timer 到期后投递 `SIGALRM` 和 `interrupt()` 唤醒 blocked task。

原实现只在用户态 timer interrupt 的 trap 路径调用 `check_all_task_timers()`。但 `TCP_CRR` 结束阶段客户端在 `sys_pselect6()` 内部用 `suspend_current_and_run_next()` 长时间轮询控制连接超时；这段时间系统在内核态反复自愿调度，不一定经过用户态 timer trap 扫描所有任务。结果是服务端 blocked `accept` 的 itimer 没有及时被检查，客户端控制连接等待先超时退出。

曾尝试在调度主循环中扫描所有 task timer，`TCP_CRR` 可以通过，但会改变 running/ready 线程自身 `SIGALRM` 的交付时机，使 `TCP_STREAM` 的控制响应等待被 `EINTR` 打断。因此修复必须只覆盖 blocked task 的唤醒需求。

## 根因

内核态自愿调度期间没有检查 blocked task 的 `setitimer`。当服务端阻塞在 `accept`，而客户端在 `pselect6` syscall 内等待控制响应时，服务端的 `SIGALRM` 唤醒可能滞后到客户端控制等待超时之后。

## 修复

新增 `check_blocked_task_timers()`，只遍历 `TaskStatus::Blocked` 的任务并调用 `check_timer()`；调度主循环 `run_tasks()` 每轮先检查 blocked task timer，再检查 futex timer。

这样可以让 blocked `accept` 的 `ITIMER_REAL` 在内核态调度循环中及时唤醒，同时不提前扫描 running/ready 线程的普通测试定时器，避免扩大 `SIGALRM` 交付时机。

后续 6.29 的 signal/itimer 重构已删除 `TaskControlBlock::check_timer()`。当前 `check_blocked_task_timers()` 仍保留 blocked task 补扫，但只负责筛选候选任务，实际到期判断由 `Timer::take_expired_signal()` 完成，`SIGALRM` 投递由 `signal::deliver_itimer_signal()` 完成。详见 [signal-itimer-refactor.md](./signal-itimer-refactor.md)。

同次重构还修复了一个后续回退：`interruptible()` 正常完成后若不清理 `interrupt_waker`，下一个 `pselect6()` 可能继承前一次 socket wait 的残留 waker，被 blocked itimer 补扫误唤醒并返回 `EINTR`。当前代码在 `interruptible()` 和 `sigtimedwait` 退出路径清理 interrupt waiter，避免重新引入 `TCP_STREAM errno 4`。

涉及文件：

| 文件 | 修改 |
|------|------|
| `os/src/task/manager.rs` | 新增 `check_blocked_task_timers()` |
| `os/src/task/processor.rs` | `run_tasks()` 中调用 blocked task timer 检查 |

## 验证

已执行：

```text
make
timeout 300s make run > log.ans 2>&1
```

结果：

```text
====== netperf UDP_STREAM end: success ======
====== netperf TCP_STREAM end: success ======
====== netperf UDP_RR end: success ======
====== netperf TCP_RR end: success ======
====== netperf TCP_CRR end: success ======
```

新的 `log.ans` 中未再出现 `recv_response_timed_n`、`errno 4`、`errno 9` 或 `errno 92`。`GetSockOpt ret = Protocol not available` 仍可能作为 netperf TCP option 探测结果出现，不影响本问题的通过判定。
