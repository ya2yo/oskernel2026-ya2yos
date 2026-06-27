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

## 2026-06-27：iperf 优化后 lmbench-musl `lat_sig` 卡住

### 背景

优化 iperf 后，LoongArch `lmbench-musl` 在运行信号延迟相关子项时无法结束。优化前 lmbench 可正常跑完，因此优先从本轮改动影响到的 `select/pselect6`、信号 ABI 和 mmap fault 语义回溯。

### 现象

`log.ans` 中 `lat_select file` 已正常退出，真正的第一处卡点出现在：

```text
./lmbench_all lat_sig -P 1 catch
```

父进程在 `pselect6` 超时后进入错误清理路径，向子进程发送 `SIGTERM` 并阻塞在 `wait4()`。子进程收到 `SIGTERM` 后没有按预期退出，而是继续 `SIGUSR1` / `pselect6` 循环，表现为死循环。

修复该问题后，继续暴露第二处卡点：

```text
./lmbench_all lat_sig -P 1 prot lat_sig
```

该子项将 `/musl/lat_sig` 以 `MAP_SHARED | PROT_READ` 映射后故意写入，只读页应触发 `SIGSEGV` / `SIGBUS` 并由用户态 handler 计数。旧日志中没有对应信号，子进程只反复执行 `getrusage/clock_gettime/getppid` 等循环。

### 分析

`lat_sig catch` 的父进程调用 `select(response, &readfds, NULL, &exceptfds, timeout)`。内核在 `pselect6` 超时时直接返回 `Ok(0)`，没有把已经清零的 ready fd_set 写回用户态。这样用户态 `exceptfds` 中保留了调用前的旧 bit，`FD_ISSET(response, &fds_error)` 被误判为异常，父进程进入 cleanup，向子进程发 `SIGTERM` 并等待。

排查过程中还发现两个会放大信号问题的兼容性错误：

- raw Linux `pselect6` 的第六参数不是直接的 `sigset_t *`，而是 `{ sigset_t *ss, size_t ss_len }`。
- LoongArch/musl 传给 `rt_sigaction` 的 raw kernel sigaction 布局是 `handler, flags, mask[2], unused`，内核原先按内部 `SigAction` 直接拷贝，导致 `sa_mask` 可能读成随机值。

`lat_sig prot` 的根因在 mmap lazy fault：`MemorySetInner::lazy_page_fault()` 找到 `MapAreaType::Mmap` 后，只按 trap 类型选择 `mmap_read_page_fault()` 或 `mmap_write_page_fault()`，没有先检查 VMA 权限。于是对未映射的 `PROT_READ` mmap 页执行 store 时，内核仍分配物理页并返回成功；用户态写指令继续执行，自然不会进入 `SIGSEGV` handler。

### 根因

本轮 lmbench 卡死由多个语义错误叠加造成：

1. `pselect6` 超时路径没有按 Linux select 语义回写 fd_set。超时返回 0 时，传入的 fd_set 应被写回为当前 ready 集合，也就是全空集合。
2. `pselect6` raw sigmask 参数解析错误，临时信号 mask 的应用/恢复和 pending signal 中断处理不完整。
3. `rt_sigaction` 用户 ABI 与内核内部结构混用，导致 musl 注册的 signal mask 不可靠。
4. mmap lazy allocation 没有按访问类型检查 `PROT_*` 权限，写只读 mmap 页被错误当作可修复缺页处理，`lat_sig prot` 收不到预期 `SIGSEGV`。

### 修复

- `os/src/syscall/io_mpx/select.rs`
  - 按 Linux raw ABI 解析 `pselect6` 第六参数 `{ ss, ss_len }`。
  - 校验 `ss_len == sizeof(SigSet)`，并从临时 mask 中移除 `SIGKILL/SIGSTOP`。
  - 在 ready、timeout 和 copy_to_user 失败路径恢复旧 signal mask。
  - 等待循环中检查未屏蔽 pending signal，并返回 `EINTR`。
  - timeout 返回 0 前也回写 read/write/except fd_set，避免用户态保留旧 bit。

- `os/src/syscall/signal.rs`
  - 增加 syscall 边界 `RawSigAction`，按 `handler, flags, mask[2], unused` 与用户态交换，再转换为内部 `SigAction`。
  - 注册 handler 时清除 `SIGKILL/SIGSTOP` mask。

- `user/src/lib.rs`
  - 同步用户库 `SigAction` 布局，避免内核自带用户程序与 syscall ABI 不一致。

- `os/src/mm/memory_set/mmap_ops.rs`
  - mmap lazy fault 按访问类型先检查 VMA 权限：load 需要 `R`，fetch 需要 `X`，store 需要 `W`。
  - 对 `PROT_READ` mmap 页的 store fault 返回 `false`，交由 trap 层发送 `SIGSEGV`，满足 `lat_sig prot` 的测试预期。

### 验证

已执行：

```text
make
make log
timeout 300s make run > log.ans 2>&1
```

结果：

- `make` 与 `make log` 通过。
- `lat_sig catch` 不再在父进程 cleanup / `wait4()` 路径卡住。
- `lat_sig prot` 中只读 mmap 写入产生预期 `StorePageFault -> SIGSEGV`，用户态 handler 能推进测试。
- `log.ans` 最终输出：

```text
#### OS COMP TEST GROUP END lmbench-musl ####
shutdown!
```
