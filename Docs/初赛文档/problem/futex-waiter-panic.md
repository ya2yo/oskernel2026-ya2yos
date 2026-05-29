# futex 信号中断后残留 Waiter 导致重复唤醒 panic

### 现象

Docker 环境中运行内核，在 futex 相关测试中出现 panic：

```c
panic
[kernel] Panicked at src/task/manager.rs:43 add task fail: task already in queue!
```

但相同的代码在本地环境中正常运行。

### 根因

`futex_wait_bitset` 被信号（如 `SIGRT_1`）中断返回 `EINTR` 时，已插入 `FUTEX_QUEUE_BITMAP` 的 `FutexWaiter` 条目没有被清理。

具体时序如下：

1. **TID 6** 调用 `futex_wait_bitset(pa)` → 创建 `FutexWaiter{A}` 插入等待队列 → `block_current_and_run_next()` 将 TID 6 置为 Blocked
2. **TID 5** 调用 `tkill` 发送 `SIGRT_1` 给 TID 6
3. **TID 6** 被信号唤醒 → `futex_wait_bitset` 检测到 pending signal → 返回 `EINTR`。**但 FutexWaiter{A} 仍然残留在 `FUTEX_QUEUE_BITMAP[pa]` 中**
4. **TID 6** 执行信号处理函数 → sigreturn 恢复上下文
5. **TID 6** 重新调用 `futex_wait_bitset(pa)` → 创建新的 `FutexWaiter{B}` 插入队列 → 再次 `block_current_and_run_next`
6. **TID 5** 调用 `futex_wake(pa)` → 遍历等待队列：
   - 弹出 `FutexWaiter{A}`（残留的旧条目）→ `wakeup_futex_task(TID 6)` → 加入 ready_queue ✓
   - 弹出 `FutexWaiter{B}` → `wakeup_futex_task(TID 6)` → TID 6 已在 ready_queue → **PANIC!**

### 为什么本地不触发

这是一个调度时序竞态。Docker 环境中 QEMU/LoongArch 模拟速度与本地不同：

- **Docker**：信号中断 → TID 6 重新进入 futex_wait → TID 5 才执行 wake（两个 Waiter 都在队列里，触发重复加入）
- **本地**：TID 5 的 wake 在 TID 6 重新进入 futex_wait 之前就到达了（队列中只有一个 Waiter，不会重复）

### 修改点

`os/src/task/futex.rs`：`futex_wait_bitset` 在检测到信号中断准备返回 `EINTR` 前，使用 `futex_key` 从 `FUTEX_QUEUE_BITMAP` 中移除当前任务的 `FutexWaiter` 条目。

```rust
// woke by signal
if !task_inner.sig_pending.difference(task_inner.sig_mask).is_empty() {
    // 清理残留的 Waiter，防止后续 futex_wake 重复唤醒
    let futex_key = task_inner.futex_key;
    let futex_pa = task_inner.futex_pa;
    drop(task_inner);
    let mut waitq = FUTEX_QUEUE_BITMAP.lock();
    if let Some(queue) = waitq.get_mut(&futex_pa) {
        if let Some(idx) = queue.iter().position(|x| x.futex_key == futex_key) {
            queue.remove(idx);
        }
    }
    return Err(SysErrNo::EINTR);
}
```

此清理模式与 `handle_timer` 中超时移除 Waiter 的逻辑一致。

### 补充修复：add_signal 与 futex_wake 竞态

上述修复只覆盖了**被阻塞任务先被调度运行**再检测信号的路径。但在非抢占内核中，存在另一条更短的竞态路径：

#### 时序

1. **TID 6** `futex_wait(pa)` → Waiter{A} 入队 → `block_current_and_run_next()` → Blocked
2. **TID 5** `tkill(SIGRT_1 → TID 6)` → `add_signal()`:
   - `sig_pending |= SIGRT_1`
   - TID 6 是 Blocked → 改 Ready → **`ready_queue::add_task(TID 6)`** ← 第一次入队
3. **TID 5** 仍在运行，调用 `futex_wake(pa)` → 遍历到残留的 Waiter{A} → `wakeup_futex_task(TID 6)`:
   - **`ready_queue::add_task(TID 6)`** ← 第二次入队 → **PANIC!**

关键在于 `add_signal`（`signal/mod.rs:240`）唤醒阻塞态任务时，**没有清理** FUTEX_QUEUE_BITMAP 中的 Waiter。TID 6 还没被调度运行（非抢占），所以第一步的 EINTR 清理代码没有机会执行。

#### 修改点

三处协同修复：

**1. `os/src/task/manager.rs` — `wakeup_futex_task`**

```rust
pub fn wakeup_futex_task(task: Arc<TaskControlBlock>) {
    let mut task_inner = task.inner_lock();
    if task_inner.task_status == TaskStatus::Ready {
        // 任务已被信号唤醒并在就绪队列中，只需清理 futex 字段
        task_inner.futex_key = 0;
        task_inner.futex_pa = 0;
        drop(task_inner);
        return;
    }
    task_inner.task_status = TaskStatus::Ready;
    task_inner.futex_key = 0;
    task_inner.futex_pa = 0;
    drop(task_inner);
    ready_queue::add_task(&task);
}
```

当 `futex_wake` / `handle_timer` / `futex_requeue` 调用此函数时，若任务已被 `add_signal` 提前唤醒（状态为 Ready），则跳过重复入队，仅清理 futex 字段。

**2. `os/src/task/futex.rs` — `new_futex_key`**

```rust
fn new_futex_key() -> usize {
    // +1 确保 key 从 1 开始，0 表示"无/已清理的 Waiter"
    FUTEX_KEY_COUNTER.fetch_add(1, Ordering::Relaxed) + 1
}
```

原来 `fetch_add` 返回旧值，第一个 key 为 0，与 `wakeup_futex_task` 中 "key=0 表示已清理" 的语义冲突。

**3. `os/src/task/futex.rs` — EINTR 清理路径增加守卫**

```rust
if futex_key != 0 {
    let mut waitq = FUTEX_QUEUE_BITMAP.lock();
    if let Some(queue) = waitq.get_mut(&futex_pa) {
        if let Some(idx) = queue.iter().position(|x| x.futex_key == futex_key) {
            queue.remove(idx);
        }
    }
}
```

当任务最终被调度运行时，`wakeup_futex_task` 可能已将 `futex_key` 清零（通过 `add_signal`→`futex_wake` 路径）。守卫避免用 key=0 误删其他 Waiter。

---
