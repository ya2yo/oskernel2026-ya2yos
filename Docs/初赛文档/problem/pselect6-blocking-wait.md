# pselect6: 从轮询等待改为 waker 阻塞

## 背景

`netperf-musl` 的控制连接依赖 `pselect6()` 等待 fd 可读。此前 `sys_pselect6()` 在内核中循环扫描 fd，未就绪时调用 `suspend_current_and_run_next()` 主动让出 CPU，本质仍是轮询等待。

## 现象

轮询版 `pselect6()` 虽能通过已有 netperf 测例，但存在几个问题：

- 等待 fd 未就绪时任务仍反复进入 ready queue，带来额外调度和 fd 扫描开销。
- 依赖调度时序，容易被 timer、信号和网络 wake 的先后顺序影响。
- 不能利用 `File::register()` / `PollSet` 现有 waker 机制精准唤醒。

## 分析

仓库中 `wait4/waitid` 已使用 `block_on(poll_fn(...))` 的阻塞模式：每次 poll 先检查条件，未满足时注册 waker 并返回 `Poll::Pending`，由事件唤醒后再次 poll。

`pselect6()` 可以沿用这个结构：

- syscall 入口仍负责解析 raw `pselect6` sigmask、复制用户 fdset 和 timeout。
- 内核 future 每轮先收集 fd 对应的 `Arc<dyn File>`，不持有 task inner 锁调用 `file.poll()`。
- 若 fd 未就绪，先处理 pending signal；忽略类信号消费后继续等待，其它可见信号返回 `EINTR`。
- Pending 前调用 `file.register(cx, interests)` 注册 waker，并二次 poll，避免事件发生在第一次 poll 与注册之间导致丢唤醒。
- timeout 复用已有 `TimerFuture/timeout()`，并在 timer interrupt 与调度循环中调用 `check_timer_events()` 触发超时 wake。

实现过程中暴露了两个调度细节：

1. `MyWaker::wake_by_ref()` 原来只要目标任务不是 `Ready` 就加入 ready queue。网络 poll 可能同步 wake 当前 `Running` 任务，导致同一 TCB 被重复入队，最终 `inner_lock()` 重入 panic。修复为只把 `TaskStatus::Blocked` 的任务放回 ready queue。
2. 之前为 TCP_CRR 轮询版补的 `check_blocked_task_timers()` 会扫描所有 blocked task 的 `ITIMER_REAL`。`pselect6` 阻塞后，客户端控制等待也变成 blocked task，若被这条兼容扫描提前投递 `SIGALRM`，`TCP_STREAM` 会回退为 `errno 4`。因此给 pselect 等待期间设置 `skip_blocked_itimer_check`，使它使用自己的 fd/timeout/signal 唤醒逻辑，同时保留 blocked `accept` 的兼容 itimer 扫描。

## 根因

旧 `pselect6()` 没有进入真正的事件阻塞路径，只是内核态轮询；直接阻塞化后又需要修正 waker 对 `Running` 任务重复入队的问题，并区分 pselect 自身等待与 netperf server blocked `accept` 的 itimer 兼容扫描。

## 修复

涉及文件：

| 文件 | 修改 |
|------|------|
| `os/src/syscall/io_mpx/select.rs` | 将 `sys_pselect6()` 改为 `block_on(poll_fn(...))`，注册 fd waker，使用 timeout future，并在返回前恢复 sigmask / 清理 pselect 标志 |
| `os/src/task/future/mod.rs` | `MyWaker` 只唤醒 `TaskStatus::Blocked` 任务，避免 Running 任务重复入队 |
| `os/src/task/task/task.rs` | 增加 `skip_blocked_itimer_check` 标志 |
| `os/src/task/manager.rs` | blocked task itimer 兼容扫描跳过设置了该标志的任务 |
| `os/src/task/processor.rs` | 调度循环检查 async timer future，同时保留 filtered blocked task itimer 扫描 |
| `os/src/trap/mod.rs` | timer interrupt 中唤醒 async timer future |

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
#### OS COMP TEST GROUP END netperf-musl ####
shutdown!
```

日志中未出现 `recv_response_timed_n`、`errno 9`、`errno 92`、`errno 4`、`Protocol not available`、`panic` 或 `ERROR`。
