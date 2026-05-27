# AI 工作日志

## 2026-05-27: futex add_signal 与 futex_wake 竞态修复

### 问题发现

用户提供 `log.ans`（约 800KB, 7168 行内核日志），要求分析报错原因。

日志末尾 panic：

```c
[kernel] Panicked at src/task/manager.rs:43 add task fail: task already in queue!
```

### 根因分析

从日志中提取关键事件序列（TID 5 与 TID 6 的交互）：

1. TID 5 `clone` 创建 TID 6，设置 `SIGRT_1` 信号处理函数（SA_RESTART）
2. TID 6 调用 `futex(FUTEX_WAIT, pa)` → 创建 Waiter 插入 `FUTEX_QUEUE_BITMAP[pa]` → `block_current_and_run_next()` 进入 Blocked
3. TID 5 调用 `tkill(SIGRT_1 → TID 6)` → `add_signal()` 检测到 TID 6 为 Blocked → 改 Ready → `ready_queue::add_task(TID 6)` ← **第一次入队**
4. TID 5 继续运行（非抢占内核），调用 `futex(FUTEX_WAKE, pa)` → 遍历 `FUTEX_QUEUE_BITMAP[pa]` 找到残留 Waiter → `wakeup_futex_task(TID 6)` → `ready_queue::add_task(TID 6)` ← **第二次入队 → PANIC**

根因位于 `os/src/signal/mod.rs:240` 的 `add_signal` 函数：向阻塞态任务发送信号时直接将其加入就绪队列，但没有清理 `FUTEX_QUEUE_BITMAP` 中对应的 Waiter 条目。

此前已有的修复（`futex_wait_bitset` 中 EINTR 返回前清理 Waiter）只能覆盖**任务先被调度运行**再检测信号的路径。在非抢占内核中，发送信号的线程 TID 5 在 `tkill` 后继续执行 `futex_wake`，TID 6 无机会运行清理代码。

### 修改内容

| 文件 | 函数/位置 | 修改 |
| ------ | ----------- | ------ |
| `os/src/task/manager.rs:71` | `wakeup_futex_task` | 新增 `task_status == Ready` 检查：若任务已被信号唤醒则在就绪队列中，跳过重复入队，仅清理 futex 字段 |
| `os/src/task/futex.rs:182` | `new_futex_key` | `fetch_add(1) + 1` 使 key 从 1 开始分配，0 稳定表示"无效/已清理" |
| `os/src/task/futex.rs:127` | `futex_wait_bitset` EINTR 路径 | 新增 `futex_key != 0` 守卫，防止 `wakeup_futex_task` 已清理后用 key=0 误操作 |

### 涉及文件

- `os/src/task/manager.rs` — 修改 `wakeup_futex_task`（+6 行）
- `os/src/task/futex.rs` — 修改 `new_futex_key` 和 EINTR 清理路径（+6 行）
- `Docs/初赛文档/problem.md` — 追加问题记录
- `ai.log` — 本文件（新建）

### 未修改文件（仅日志/调试用途）

- `os/src/task/futex.rs:206` — `[sys_futex] strong_count` 日志注释化（已在之前修改中）
- `os/src/task/mod.rs:129` — `exit_current_group_and_run_next` 新增 debug 日志（已在之前修改中）
- `user/src/bin/initproc.rs` — 启用多组测例（已在之前修改中）
