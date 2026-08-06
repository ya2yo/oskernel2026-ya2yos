# BuildStorm futex 队列锁阻塞 remote TLB ACK

## 背景

all-hart CFS 和共享地址空间 SMP 已让同一 `rustc` 线程组在多个 Hart 上运行。新的 `server.ans` 在进入
`OS COMP TEST GROUP START buildstorm` 后长时间没有继续输出，维护者提供的 `client.ans` 包含 8 个 Hart 的 GDB 栈。

## 现象

GDB 显示：

- hart 4 在 `mprotect -> MemorySet::with_mut -> remote_tlb::shootdown()` 等待 mailbox ACK；
- hart 3 在 `remote_tlb::lock_updates()` 等待全局 `UPDATE_LOCK`；
- hart 0、2 在 `MemorySet::get_ref()` 等待同一地址空间的读锁，调用点分别落在 `clone3` 和 futex 用户值读取；
- hart 1 在 `futex_wait_bitset()` 的 `FUTEX_QUEUE_BITMAP.lock()` 自旋；
- hart 5--7 位于 `wfi`，属于当前没有可执行候选任务的 idle Hart，不是阻塞链上的根因。

## 分析

`futex_wait_bitset()` 原先先获取全局 futex 队列锁，再调用 `copy_from_user_val()` 检查用户 futex 值。该复制需要获取 `MemorySet` 读锁。与此同时，另一个 Hart 持有同一 `MemorySet` 写锁修改 PTE，并在写锁内发起 remote TLB shootdown，等待所有 active Hart ACK。

因此现场可能形成以下等待环：

`MemorySet` 写锁 -> shootdown ACK -> 等待 `FUTEX_QUEUE_BITMAP` -> futex 队列锁持有者 -> `MemorySet` 读锁。

此前只在 `UPDATE_LOCK` 和 `MemorySet::get_ref()` 的自旋中轮询 mailbox，普通 futex 队列锁等待没有轮询；当等待队列的 Hart 正好是 shootdown target 时，它无法 ACK，writer 也无法释放写锁。

## 根因

futex 队列锁的临界区跨越用户内存访问，违反 task/MM 锁边界；同时其裸自旋等待不响应 remote TLB mailbox，导致共享地址空间写锁与 futex 队列锁形成跨 Hart 的等待环。

## 修复

- 在 `os/src/task/futex.rs` 增加 `lock_futex_queue()`，使用 `try_lock()` 竞争失败时调用 `remote_tlb::poll()`，保证 futex 锁等待者可以执行本地 TLB 失效并 ACK。
- 增加 `FUTEX_QUEUE_VERSION`。futex wait 在队列锁外检查用户值，获取队列锁后验证版本；wake、requeue、timer 和清理路径递增版本。版本变化时重新检查用户值，避免将用户内存读放回 futex 锁临界区并保持 compare-and-block 的唤醒顺序。
- 所有 futex 队列获取路径统一使用轮询锁，包括 wait、wake、requeue、超时、信号清理和 robust timer 清理。

## 涉及文件

- `os/src/task/futex.rs`
- `os/src/mm/remote_tlb.rs`（现有 mailbox/poll 协议，被 futex 锁等待复用）

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- `make perf TARGET_ARCH=riscv64`：通过。
- `rustfmt --edition 2021 os/src/task/futex.rs`、`git diff --check`：通过。
- 新 `server.ans` 已确认 8 个 Hart 在线、`sigaltstack regression: PASS`、`rseq regression: PASS`，卡点位于 BuildStorm 启动后的上述等待环；修复后尚未重新执行完整 BuildStorm/LTP，不能据此宣称端到端通过。
