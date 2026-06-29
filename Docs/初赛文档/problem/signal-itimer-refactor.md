# 信号与 itimer 职责重构

## 背景

此前为了修复 `netperf TCP_CRR`，调度循环增加了对 blocked task 的 `ITIMER_REAL` 补扫，使阻塞在 `accept()` 的 netserver 能及时收到 `SIGALRM` 并返回测试结果。随后 `pselect6()` 从轮询改为 waker 阻塞等待时，又为了避免客户端控制连接也被这条补扫提前打断，在 `TaskControlBlockInner` 中加入了 `skip_blocked_itimer_check`。

这个字段只是 Ya2yOS 内部兼容开关，不属于 Linux ABI，也不对应 Linux `task_struct` 的真实语义。它把 `pselect6`、blocked task 补扫、itimer 到期判断和信号投递耦合在 TCB 中，后续继续扩展 signal 或 timer 时容易再次引入时序特判。

## 现象

当前代码存在两个明显问题：

- `TaskControlBlockInner::skip_blocked_itimer_check` 是为单个 syscall 场景添加的临时状态，语义上不应该由 TCB 长期持有。
- `TaskControlBlock::check_timer()` 同时负责读取 itimer、判断到期、重装周期 timer、投递 `SIGALRM` 和唤醒 blocked task，导致 task 模块承担了 timer 和 signal 两个领域的职责。

`log.ans` 进一步暴露了一个时序问题：移除 `skip_blocked_itimer_check` 后，`TCP_STREAM` 中客户端在数据连接 `RecvFrom` 返回 `ENOTCONN` 后进入控制连接 `pselect6()`，但随后 `pselect6` 被 `SIGALRM` 打断，用户态打印 `recv_response_timed_n: no response received. errno 4 counter -1`。这说明问题不能只靠把跳过标志从 TCB 搬到别处解决。

## 分析

Linux 中 `setitimer(ITIMER_REAL)` 的职责可以拆成两层：

- timer 层保存 `itimerval`，判断当前时间是否到达下一次到期点，并在周期 timer 到期后重装下一次到期时间。
- signal 层在 timer 到期时向目标 task 投递 `SIGALRM`，并复用统一的 pending signal / blocked task 唤醒逻辑。

TCB 只需要持有 per-thread timer 对象和信号表，不应该知道某个 syscall 是否要跳过 blocked itimer 扫描。`pselect6()` 自身是否被信号打断，应由 pending signal 可见性和等待路径决定，而不是由 TCB 中额外字段影响 timer 扫描。

结合失败窗口继续分析后，实际根因是 `interruptible()` 在普通 I/O 条件完成时没有清理 `interrupt_waker`。例如 `TcpSocket::recv()` 曾经注册过可中断等待 waker，即使后续因 socket 状态变化返回 `ENOTCONN`，这个 waker 仍留在 TCB 中。随后同一线程进入 `pselect6()` 并阻塞时，调度循环的 blocked itimer 补扫通过残留 waker 误判它仍处于可中断 socket wait，于是给客户端投递 `SIGALRM`，导致 `pselect6` 返回 `EINTR`。

因此本次重构的目标是：

- 去掉 `skip_blocked_itimer_check`，不再在 `pselect6()` 入口和退出处设置临时标志。
- 将 itimer 到期和重装逻辑收敛到 `timer::Timer`。
- 将 `SIGALRM` 投递收敛到 `signal` 模块。
- 让 task manager / trap 只调用 signal 模块的统一入口，不再直接执行 timer 判断或信号投递。
- 让 interruptible wait 的 waker 生命周期随等待结束清理，避免后续非 interruptible syscall 继承残留中断状态。

## 根因

根因不是 `pselect6` 本身需要特殊字段，而是两类职责混在了 TCB 中：

- `ITIMER_REAL` 的到期判断和 `SIGALRM` 投递被放在 `TaskControlBlock::check_timer()` 中，任务管理层为了区分不同等待场景只能继续向 TCB 添加临时状态。
- `interrupt_waker` 注册后没有随 `interruptible()` 等待结束清理，导致后续 `pselect6` 被 blocked itimer 补扫误识别成可中断 socket wait。

模块边界不清和 waker 生命周期不完整共同造成了 Linux 不兼容字段，以及移除该字段后的 `TCP_STREAM errno 4` 回退。

## 修复

涉及文件：

| 文件 | 修改 |
|------|------|
| `os/src/timer/itimerval.rs` | 移除 `TimerInner::once` 和分散的 setter，新增 `set_itimer()` 与 `take_expired_signal()`，由 timer 模块负责到期推进和单次禁用；周期 timer 保持既有 `it_interval` cadence，避免改变 netperf 依赖的 SIGALRM 时序 |
| `os/src/signal/mod.rs` | 新增 `deliver_itimer_signal(task)` / `deliver_blocked_itimer_signal(task)`，在 timer 到期时投递 `SIGALRM`，并只唤醒真正注册了 interruptible waiter 的 blocked task；新增 pselect itimer waiter guard，将等待登记放在 signal 模块 |
| `os/src/task/task/task.rs` | 删除 `skip_blocked_itimer_check` 字段和 `TaskControlBlock::check_timer()`，新增 `wake_interruptible()`、`has_interruptible_waiter()`、`clear_interrupt_waiter()` 管理 interrupt waker 生命周期 |
| `os/src/task/future/mod.rs` | `interruptible()` 返回前清理本次注册的 interrupt waker，避免残留 waker 污染后续 syscall |
| `os/src/syscall/signal.rs` | `sigtimedwait` 超时路径同样清理 interrupt waiter |
| `os/src/task/manager.rs` | blocked task 补扫只筛选 `TaskStatus::Blocked`，具体到期判断和信号投递交给 signal/timer 模块 |
| `os/src/trap/mod.rs` | timer interrupt 返回路径改为调用 `deliver_itimer_signal()` |
| `os/src/syscall/time.rs` | `setitimer()` 改为调用 `Timer::set_itimer(new_timer, now)` 安装新 timer |
| `os/src/syscall/io_mpx/select.rs` | 删除 `pselect6()` 对 `skip_blocked_itimer_check` 的设置和清理，改用 signal 模块的 pselect itimer guard，并在返回 `EINTR` 前重新检查 fd ready |

新的职责划分为：

- `timer` 模块：保存和推进 `Itimerval` 状态，判断是否有一个 `ITIMER_REAL` 到期事件可消费。
- `signal` 模块：把到期事件转换为 `SIGALRM` pending signal，并决定 blocked task 是否存在可中断 waiter。
- `task` 模块：只负责遍历当前任务或 blocked task，不再保存 syscall 级别的 timer 跳过状态。

## 验证

已执行：

```text
make
make log
timeout 300s make run > log.ans 2>&1
```

结果：

- `make` 通过。
- 当前单跑 `netperf-musl`，五个子项均通过：

```text
====== netperf UDP_STREAM end: success ======
====== netperf TCP_STREAM end: success ======
====== netperf UDP_RR end: success ======
====== netperf TCP_RR end: success ======
====== netperf TCP_CRR end: success ======
#### OS COMP TEST GROUP END netperf-musl ####
shutdown!
```

- 关键字扫描未发现 `panic`、`panicked`、`TFAIL`、`TBROK`、`ERROR`、`recv_response_timed_n`、`errno 4`、`errno 9`、`errno 92`、`select failure` 或 `Connection refused`。
- 日志中仍可见 netperf 探测 TCP option 时的 `GetSockOpt ret = Protocol not available`，该返回不影响测试判定；本次修复关注的是 `pselect6 EINTR` / `recv_response_timed_n errno 4` 回退。
