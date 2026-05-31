# clone05: CLONE_VFORK 挂起机制

### 背景

LTP `clone05` 测试 `clone(CLONE_VM | CLONE_VFORK | SIGCHLD, ...)`：父进程在 vfork 后应被挂起直到子进程 exit/exec，之后检查子进程写入的 `child_exited` 标志为 1。

### 现象

```
clone05.c:48: TFAIL: child_exited retval 0 != 1: SUCCESS (0)
```

父进程在 vfork 后立即继续执行，读到 `child_exited == 0`。

### 分析

**内核完全未处理 CLONE_VFORK**：`CloneFlags` 中定义了该标志但 `clone_process` 未检查，导致父进程创建子进程后立即返回用户态。

第一轮修复在 `clone_process` 中加 `VforkBlocked` 挂起和 exit/exec 唤醒后，测试仍失败。日志显示三处调度路径绕过了 VforkBlocked：

1. **`suspend_current_and_run_next`**：timer 中断无条件将当前任务设回 `Ready`
2. **`run_tasks`**：切换任务时无条件 `ready_queue::add_task(cur_task)`
3. **空队列路径**：VforkBlocked 仍被重新运行

### 修复

**数据结构（task.rs）：**
- `TaskStatus` 新增 `VforkBlocked` 状态
- `TaskControlBlockInner` 新增 `vfork_wait_child: usize`（0 = 未等待）

**VFORK 挂起（task.rs clone_process）：**
```
CLONE_VFORK → parent.vfork_wait_child = child.tid()
            → parent.task_status = VforkBlocked
```

**VFORK 唤醒（task/mod.rs exit_current_and_run_next + task.rs exec）：**
子进程 exit/exec 时，遍历父进程 task 列表，匹配 `vfork_wait_child` 后设回 `Ready` 并入 ready_queue。

**调度器三路径保护：**

| 文件 | 路径 | 修改 |
|------|------|------|
| `task/mod.rs:85` | `suspend_current_and_run_next` | VforkBlocked 不改回 Ready |
| `task/processor.rs:86-89` | `run_tasks` 切换路径 | VforkBlocked 不重新入队 |
| `task/processor.rs:91-99` | `run_tasks` 空队列路径 | VforkBlocked drop + continue |

### 涉及文件

- `os/src/task/task/task.rs` — 数据结构 + VFORK 挂起 + exec 唤醒
- `os/src/task/mod.rs` — exit 唤醒 + 调度路径保护
- `os/src/task/processor.rs` — 调度器入队 + 空队列路径保护

### 验证

RISC-V 单跑 clone05：父进程挂起到子进程退出后正确唤醒，`child_exited == 1` TPASS。
