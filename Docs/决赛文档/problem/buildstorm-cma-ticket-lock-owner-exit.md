# BuildStorm CMA ticket 锁 owner 退出泄漏

## 背景

BuildStorm 使用并发 Cargo 编译和大量 ext4 写入。VirtIO block I/O 的 DMA buffer 以及退出任务的页帧回收都会
调用 CMA 连续物理页分配器。旧实现以 `buddy_system_allocator::LockedHeap` 保护 `Heap`；其 vendored
`spin 0.7` 依赖为 ticket mutex。

## 现象

新的 `server.ans` 在 `Building 53/446` 后没有新的 Cargo 输出，且没有 compiler error、`panic`、`TFAIL`、
`TBROK`、测试结束或 `shutdown!`。这只是 Cargo 的最后可见进度，不能说明第 53 个 crate 编译失败。

`client.ans` 的 GDB 快照显示：

- CPU#7 在 `CMA_ALLOCATOR` lock 等待，调用链为 `cma_alloc -> virtio::share -> VirtIOBlk::write_blocks`
  `-> Disk::write_at -> ext4_fwrite -> sys_write`；
- CPU#5 在同一 lock 等待，调用链为 `cma_dealloc -> PageCache::dealloc -> FrameTracker::drop`
  `-> recycle_data_pages -> exit_current_and_run_next -> handle_signal(SIGKILL)`；
- 其余 hart 位于 `idle_until_runnable -> wfi`。

因此，Cargo 的 ext4 写入无法获得 DMA 连续页，`sys_write` 不返回，表面上表现为停在 `53/446`。

## 根因

ticket mutex 先递增 `next_ticket`，然后所有 waiter 轮询 `next_serving`；只有持锁 guard 的 `Drop` 才会推进
`next_serving`。Ya2yOS 的 `exit_current_and_run_next()` 是发散退出路径，会抛弃任务内核栈而不保证遗留 RAII
guard 析构。若退出任务是 CMA owner，或它已经领取 ticket 后退出，后续 ticket 均可能永久不能服务。

旧 GDB 现场没有保留原始 `next_ticket`、`next_serving` 或已退出 owner 的 tid，不能断言哪一个 task 首先遗留；
但 owner/已取 ticket waiter 的任一遗留均足以使 ticket lock 永久停滞。

## 修复

- 用私有 `CmaAllocator` 替换 `LockedHeap`：`AtomicUsize owner` 保护 `UnsafeCell<Heap>`，task owner 编码为
  `tid + 1`，`0` 表示未持有；启动等没有 current task 的路径使用内部 kernel sentinel。
- 获取锁使用 CAS 加短临界区自旋，不分配 ticket，也不经过 `block_on` 或可睡眠锁。CMA 可由页帧回收和驱动路径调用；
  在这些上下文引入可睡眠锁会重新带来 `MemorySet`/分配器/文件系统锁序风险。
- 私有 `CmaGuard` 只在 CAS 成功后访问 `Heap`，其 `Drop` 以 owner CAS 解锁。`UnsafeCell` 的并发安全性仅建立在该
  guard 覆盖的短 Heap 操作上，guard 不向调用者暴露。
- 在 `exit_current_and_run_next()` 取下当前 task 后、其它资源清理前调用
  `cancel_cma_lock_owner(curr_task.tid())`。若退出 task 仍是 owner，则释放锁并输出 warning；此时 task 已脱离调度，
  不会再恢复并访问被释放的临界区。

## 涉及文件

- `os/src/mm/frame_alloc/buddy_cma.rs`
- `os/src/mm/frame_alloc/mod.rs`
- `os/src/mm/mod.rs`
- `os/src/task/mod.rs`

## 验证

已执行 `rustfmt --edition 2021` 和 `git diff --check`，均通过。`make TARGET_ARCH=riscv64` 完成 user build，
但在 lwext4 CMake 配置时因宿主未安装 `riscv64-linux-musl-cc` 失败，因而没有生成可用于本修复的 guest 镜像。

上述 GDB 是旧 `LockedHeap` 实现的诊断证据，新 `CmaAllocator` 尚未在 BuildStorm 中运行。尚待 Docker/交叉工具链环境
完成 RISC-V BuildStorm（至少确认越过 `53/446`）、全程日志/GDB、LTP、`e2fsck -fn` 和 LoongArch64 回归；因此不能
宣称该停滞已经被运行时验证消除。
