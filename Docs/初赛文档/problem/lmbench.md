# lmbench

[WARN] [HART0] [PID 4] [TID 4] [kernel] hart 0 Exception(StorePageFault) in application, bad addr = 0x2a23446000, bad instruction = 0x1b384, kernel killed it.
[WARN] [HART0] [PID 4] [TID 4] don't send SIGSEGV, just exit the process

根据 cursor 回答：这是 lmbench 的保护异常测试：它故意向只读 mmap 页写入，期望内核产生 SIGSEGV / SIGBUS，然后用户注册的 signal handler 捕获它，用来测量 protection fault 开销。

原先的机制是直接退出当前进程，但是原来的作者有实现发送信号的代码，只是没有启用，通过判断进程是否注册sig_handler来发送信号，否则还是直接exit。
改完后发现还是死循环，根据gdb的结果，sepc的值没有发生改变，推测信号处理函数异常。
继续分析后发现，`Protection fault` 本身不是最后卡住的位置。

后续继续分析发现，`Protection fault` 本身已经完成，真正卡住的是脚本下一项：

```sh
./lmbench_all lat_pipe -P 1
```

`lat_pipe` 会 fork 出父子进程，用两根 pipe 做 ping-pong 往返测试。原来的 pipe 实现中，空读或满写时只是调用 `suspend_current_and_run_next()`。这个函数只会把当前任务重新置为 `Ready`，等价于 yield，不是真正阻塞等待。结果父子进程在空 pipe 上反复进入内核轮询，`Protection fault` 后看起来像死循环，实际上是卡在 `lat_pipe` 的大量 pipe 往返中，长时间跑不出下一项。

本次修改点：

- `os/src/fs/files/pipe.rs`
  - 为 pipe ring buffer 增加 `read_waiters` 和 `write_waiters`。
  - 读空 pipe 时，把当前任务置为 `Blocked` 并放入读等待队列；写入数据后唤醒读者。
  - 写满 pipe 时，把当前任务置为 `Blocked` 并放入写等待队列；读取数据后唤醒写者。
  - 入队前后复查 pipe 状态，避免“刚检查为空/满，对端马上写入/读取”导致丢唤醒。
  - 阻塞前检查 pending signal，有信号则返回 `EINTR`，避免信号无法处理。

- `os/src/task/mod.rs`
  - 增加 `schedule_blocked_current()`，用于已经由调用者设置为 `Blocked` 的任务直接切回调度器，避免 pipe 等待队列需要重复设置状态。

- `os/src/signal/mod.rs`
  - 给阻塞态任务发送信号时，将其改回 `Ready` 并放回 ready queue。否则 `SIGKILL` 等信号可能只进入 `sig_pending`，任务仍停在 pipe/futex 等等待队列中，无法回到 trap 返回路径处理信号。

- `os/src/arch/riscv64/qemu/page_table.rs`
  - 同步 LoongArch 已修复过的 COW 处理：进入 COW 时清 `WRITEABLE | DIRTY`，解除 COW 时恢复 `WRITEABLE | DIRTY` 并刷新 TLB。
  - 这可以避免 fork 后父子进程继续通过旧 writable/dirty TLB 或页表状态写共享页，污染 busybox/lmbench 的用户态状态。

验证：

- `make build-arch TARGET_ARCH=riscv64` 通过。
- `timeout 90s make run` 中已经可以越过原先卡点，输出：

```text
Protection fault: ...
Pipe latency: ...
Process fork+exit: ...
Process fork+execve: ...
```
