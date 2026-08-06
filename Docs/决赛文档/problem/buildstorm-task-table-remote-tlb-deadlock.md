# BuildStorm 任务表锁阻塞 remote TLB ACK

## 背景

修复 futex 队列锁等待环后，新的 `server.ans` 仍停在
`OS COMP TEST GROUP START buildstorm`。维护者提供的 `client.ans` 是该次卡住现场的 GDB 多 Hart 栈。

## 现象

GDB 显示 hart 1 在 `mprotect -> MemorySet::with_mut -> remote_tlb::shootdown()` 等待 ACK，hart 2 在同一
地址空间的 `MemorySet::get_ref()` 读锁上等待，hart 0、3--6 在 `check_blocked_task_timers()` 或
`sched_getaffinity()` 路径进入 `tid_to_task` 的 `TID_TO_TASK.lock()`。hart 7 停在用户陷阱入口断点，说明
现场是全局任务表锁争用与地址空间读写锁并发，不是 futex 队列锁本身。

## 分析

`remote_tlb::shootdown()` 在持有 `MemorySet` 写锁时等待目标 Hart 的 mailbox ACK。目标 Hart 如果在
`TID_TO_TASK.lock()` 裸自旋，就无法执行 `remote_tlb::poll()`，因此不能确认 ACK；写 Hart 也就无法释放
地址空间写锁，其他需要 `MemorySet` 读锁的 Hart 随之排队。

此前 `TID_TO_TASK` 的所有入口都直接调用 `Mutex::lock()`，与已经修复的 `UPDATE_LOCK`、`MemorySet` 读锁和
futex 队列锁不同，等待时不会处理远程 TLB mailbox。

## 根因

全局任务表的自旋等待没有遵守跨 Hart shootdown 协议。另一个潜在问题是 `get_all_tasks()` 在任务表 guard
仍存活时执行 `Vec::collect()`，形成 `TID_TO_TASK -> HEAP` 锁顺序，可能与任务退出路径反向等待。

## 修复

- 在 `os/src/task/manager.rs` 增加 `lock_task_table()`，使用 `try_lock()` 失败时调用
  `crate::mm::remote_tlb::poll()` 并执行 `spin_loop()`。
- `tid2task`、`insert`、`remove`、`task_num`、`get_all_tasks` 和 `for_each_task` 统一使用该 helper。
- `get_all_tasks()` 改为复用 `for_each_task()`；任务表锁内只 clone 单个 `Arc`，`Vec` 扩容和调用方处理均在锁外。

## 涉及文件

- `os/src/task/manager.rs`
- `server.ans`
- `client.ans`

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- `make build-arch TARGET_ARCH=riscv64 SCHEDULER=rr`：通过。
- `make perf TARGET_ARCH=riscv64`：通过。
- `rustfmt --edition 2021 os/src/task/manager.rs`、`git diff --check`：通过。
- 完整 BuildStorm/LTP 尚未重跑；当前正式 `disk.img` 仍可能被维护者 QEMU 占用，不能据此宣称运行期卡死已消失。
