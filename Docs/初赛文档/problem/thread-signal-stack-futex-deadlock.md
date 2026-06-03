# 线程信号栈检查错误与 futex 退出死锁

## 背景

LTP `pthread_cancel_points` 测例执行流程：

1. 主线程（TID 3）通过 `clone(CLONE_THREAD, stack=mmap_addr)` 创建子线程（TID 4）
2. TID 4 在 futex 取消点阻塞
3. TID 3 发送 `SIGRT_1`（cancellation signal）给 TID 4，并通过 futex wake 唤醒它
4. TID 4 应处理信号 → `pthread_exit` → 写入 join futex → 唤醒 TID 3
5. TID 3 在 `pthread_join` 中等待

实际问题：TID 4 在信号处理时被内核错误终止，TID 3 永久阻塞。

## 现象

```
[PID 3] [TID 4] [handle_signal] signo=33, handle signal SIGRT_1
[WARN] setup_frame: user stack too small for signal 33, sp=0x2a234469d0, stack_bottom=0x2ffe447000
[PID 3] [TID 4] [exit_current_and_run_next] enter!
[PID -1] [TID -1] [handle_futex_when_exit] robust_list.list is 0, nothing to do
[PID -1] [TID -1] TCB 4 dropped
// 此后 TID 3 永久阻塞在 futex_wait，无人唤醒
```

线程栈 SP=0x2a234469d0（mmap 区域 0x2a23446000-0x2a23469000 内），但内核使用的主线程栈边界 0x2ffe447000 与之完全不匹配，导致栈空间检查失败。

## 两个 bug

### Bug 1: `setup_frame` 栈边界检查使用错误的 `user_stack_top`

**根因**：`setup_frame` 原本通过 `task_inner.user_stack_top - USER_STACK_SIZE` 计算栈底。`user_stack_top` 在 `alloc_user_res()` 中被设置为内核分配的栈区域顶部（靠近 `USER_STACK_TOP`），对所有线程都一样。

但对于 `CLONE_THREAD` + 用户提供栈的线程：
- `trap_cx.sp` 指向用户提供的 mmap 栈（0x2a23446ad0）
- `user_stack_top` 仍然指向内核分配的主栈区域（~0x2ffe447000）
- SP 所在 mmap 区域与 `user_stack_top` 指向的栈区域完全不同

导致栈空间检查 `sp < stack_bottom + min_frame_size` 使用错误的边界，判定"栈太小"而异常终止。

**修复**：`setup_frame` 改为动态查找：从 `memory_set.areas` 中按当前 SP 定位包含它的 `MapArea`，以该区域起始地址作为栈底。无论主线程栈还是 mmap 线程栈都能正确工作。回退逻辑使用 `sp - USER_STACK_SIZE` 作为保守估计。

### Bug 2: 线程退出时未唤醒被 futex 阻塞的兄弟线程

**根因**：`exit_current_and_run_next` 的清理逻辑只处理：
1. `clear_child_tid`（写入 0 + futex wake）—— TID 3 等待的 futex 地址（0x2a23446b30）并非 `clear_child_tid`
2. robust futex 链表 —— TID 4 的 `robust_list.list` 为 0，无操作

TID 3 在 futex_wait 中处于 Blocked 状态，无人将其唤醒，进程死锁。

**修复**：在 `exit_current_and_run_next` 中，线程被标记为 Zombie 后、进程级清理前，新增兄弟线程遍历逻辑：将处于 Blocked 状态的兄弟线程从 futex 等待队列中移除、清理 futex 状态、加入就绪队列。

### 重构: 移除 `user_stack_top` 字段

问题的根源之一是在 `TaskControlBlockInner` 中冗余保存 `user_stack_top`。栈的权威信息本就在 `memory_set.areas`（`MapAreaType::Stack`）中。

所有原来读取 `user_stack_top` 的场景均改为从 `memory_set` 动态查找：

| 原使用场景 | 替代方案 |
|-----------|---------|
| initproc 设置初始 SP | `alloc_user_res()` 返回值 |
| execve argv/envp 压栈 | `alloc_user_res()` 返回值 |
| fork 子进程栈复制 | 子进程 `memory_set` 中查找 `Stack` 区域 |
| exit 栈回收 | `memory_set` 中查找 `Stack` 区域后 remove |
| setup_frame 栈边界 | 按 SP 定位包含它的任意 `MapArea` |

`alloc_user_res` 签名从 `fn(_, &mut TaskControlBlockInner)` 改为 `fn(_, &mut TaskControlBlockInner) -> usize`，返回栈顶地址。

CLONE_THREAD + 用户栈时使用 `alloc_trap_context_only()`（仅分配 trap context，不分配内核栈），自然不会在 `memory_set` 中留下 `Stack` 类型区域，exit 时也无需特殊处理。

## 涉及文件

| 文件 | 修改内容 |
|------|---------|
| `os/src/signal/mod.rs` | `setup_frame`：按 SP 动态查找 MapArea 确定栈底；回退值改用 `sp - USER_STACK_SIZE`；移除对 `user_stack_top` 的依赖 |
| `os/src/task/task/task.rs` | 移除 `user_stack_top` 字段；`alloc_user_res` 返回栈顶；新增 `alloc_trap_context_only`；fork/initproc/execve 调用方适配 |
| `os/src/task/mod.rs` | exit 栈回收改为查找 `Stack` 区域；新增兄弟线程唤醒逻辑；清理无用 import |
| `os/src/task/futex.rs` | `FUTEX_QUEUE_BITMAP` 改为 pub 供外部清理使用 |
