# netperf: select 误被 SIGCHLD 打断

## 背景

LoongArch 单跑 `netperf_testcode.sh` 时，`netserver` 需要在多个 netperf 子项之间持续监听控制端口。日志来自 `log.ans`。

## 现象

原始日志中 `UDP_STREAM` 本身完成，但服务端随后打印：

```text
accept_connections: select failure: Interrupted system call (errno 4)
```

`netserver` 因 `select()` 返回 `EINTR` 退出，后续 `TCP_STREAM`、`UDP_RR`、`TCP_RR`、`TCP_CRR` 都无法连接控制端口并失败。

## 分析

`sys_pselect6()` 在阻塞等待前直接检查 `sig_pending.difference(sig_mask)`。只要存在未屏蔽 pending signal，就返回 `EINTR`。

netperf 运行过程中子进程退出会给父进程留下 `SIGCHLD`。`SIGCHLD` 的默认动作是 Ignore，已有 `wait*` 路径也会消费默认忽略或显式忽略的信号后继续等待。但 `pselect6/ppoll` 没有沿用这套语义，于是把默认忽略的 `SIGCHLD` 错误升级成用户可见 `EINTR`。

继续用 GDB 定位 `TCP_CRR` 压力段 panic 时，发现另一个 `select` 问题：`sys_pselect6()` 在持有当前任务 `inner_lock` 时调用 `file.poll()`。TCP/loopback poll 可能触发 `PollSet::wake()`，进而调用 `MyWaker::wake_by_ref()` 再次锁当前 TCB，形成锁重入。

## 根因

1. `pselect6/ppoll` 未区分“真正应中断 syscall 的信号”和“默认忽略/显式忽略的信号”。
2. `pselect6` 的任务锁范围过大，覆盖了会唤醒 waker 的 `file.poll()` 网络路径。

## 修复

- `sys_pselect6()` / `sys_ppoll()` 遇到 `SIGCHLD`、`SIG_IGN` 或默认动作为 Ignore 的 pending signal 时，消费 pending bit 并继续等待；其他信号仍返回 `EINTR`。
- `sys_pselect6()` 轮询 fd 时不再持有 `task.inner_lock()`；只在恢复 signal mask 和检查 pending signal 时短暂加锁。
- `TaskControlBlock::check_timer()` 中显式释放读取 `task_status` 的锁后再投递 `SIGALRM`，避免同类锁生命周期误用。

## 涉及文件

- `os/src/syscall/io_mpx/select.rs`
- `os/src/syscall/io_mpx/poll.rs`
- `os/src/task/task/task.rs`

## 验证

已执行：

```text
make
timeout 300s make run > /tmp/netperf-after-real-select-fix.log 2>&1
```

结果：

- `make` 通过。
- 原始失败点消失，日志中不再出现 `accept_connections: select failure: Interrupted system call`。
- `netperf-musl` 前四项结果从原始后续连接失败推进为：

```text
====== netperf UDP_STREAM end: success ======
====== netperf TCP_STREAM end: success ======
====== netperf UDP_RR end: success ======
====== netperf TCP_RR end: success ======
```

剩余 `TCP_CRR` 仍失败：

```text
recv_response_timed_n: no response received. errno 9 counter 0
====== netperf TCP_CRR end: fail ======
```

该失败发生在原始 `netserver` 提前退出问题修复之后，并伴随 LoongArch `Unknown` trap 日志，属于后续网络/架构陷入路径问题，未纳入本次修复范围。
