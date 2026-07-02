# clone02: CLONE_SIGHAND 共享 SigTable 导致退出路径重入锁

## 背景

LTP `clone02` 会覆盖 `clone()` 中 `CLONE_VM | CLONE_SIGHAND | SIGCHLD`
这类组合。该组合创建的是独立子进程，不是 `CLONE_THREAD` 线程；父子可以共享信号处理动作表，但仍有独立的进程生命周期、退出码和 wait 状态。

## 现象

单跑 `clone02` 时内核在退出路径 panic：

```text
RUN LTP CASE clone02
["clone02\0"]
panic
[kernel] Panicked at src/task/process/process.rs:295 You should not fail to get lock in a 1 HART system!
```

GDB 回溯显示 panic 发生在子进程 `exit_group()` 后的退出清理中：

```text
ProcessInner::get_locked_sigtable()
send_signal_to_thread_group()
exit_current_and_run_next()
exit_current_group_and_run_next()
sys_exit_group()
```

对应源码位置是 `exit_current_and_run_next()` 中进程最后一个线程退出后，向父进程发送 `exit_signal`（通常是 `SIGCHLD`）：

```rust
let sigtable = curr_proc.get_locked_sigtable();
if !sigtable.is_exited() {
    sigtable.set_exit_code(exit_code);
}
...
send_signal_to_thread_group(curr_task.ppid(), SigSet::from_sig(exit_signal as usize));
```

## 分析

原实现把两类不同语义都放在 `SigTable` 中：

- signal action 表，受 `CLONE_SIGHAND` 影响，可以在不同进程间共享。
- `group_exit_code`，表示线程组/进程退出状态，只应属于某个 `Process`。

当 `clone02` 使用 `CLONE_SIGHAND` 创建独立子进程时，父子进程的 `ProcessInner.sig_table` 指向同一个 `Arc<Mutex<SigTable>>`。子进程退出时先持有这把 `sig_table` 锁写入退出码；随后退出路径向父进程发送 `SIGCHLD`，`send_signal_to_thread_group(parent_pid, SIGCHLD)` 又会取父进程的 `sig_table` 来判断父进程是否已经退出。

由于父子共享的是同一个 `SigTable`，这等价于在同一控制流里二次 `try_lock()` 同一把锁。当前内核运行在 1 HART 上，`try_lock().expect()` 直接触发 panic，所以表面看是“死锁”，实际是锁重入被 `expect()` 转成了 panic。

即使只在调用 `send_signal_to_thread_group()` 前 `drop(sigtable)`，也只能绕过 panic，不能修复语义问题：子进程写入共享 `SigTable.group_exit_code` 后，父进程也会被错误标记为已经退出。

## 根因

`SigTable` 同时承载了“可被 `CLONE_SIGHAND` 共享的信号处理动作”和“不能共享的线程组退出码”。这导致：

1. `CLONE_SIGHAND` 的父子进程共享了不该共享的退出状态。
2. 子进程退出通知父进程时，在同一把 `sig_table` 锁上发生重入。
3. 退出码污染父进程状态，后续调度、wait 和信号路径都可能误判父进程已退出。

## 修复

将线程组退出状态从 `SigTable` 拆出，放入 `ProcessMeta`：

- `SigTable` 只保存 signal action 表。
- `ProcessMeta` 新增 `group_exit_code: Option<i32>`。
- `Process` 新增 `is_group_exiting()`、`group_exit_code()`、`set_group_exit_code_once()`。
- `exit_current_group_and_run_next()` 使用 `set_group_exit_code_once()` 判断是否第一次发起线程组退出。
- `exit_current_and_run_next()` 最后一个线程退出时写入当前进程自己的 `group_exit_code`，再给父进程发送 `SIGCHLD`。
- `waitpid()/waitid()` 从 child 的 `ProcessMeta` 读取退出码。
- `send_signal_to_thread_group()` 判断目标进程是否正在退出时不再锁 `sig_table`。

## 涉及文件

| 文件 | 修改内容 |
|------|----------|
| `os/src/signal/sigact.rs` | 从 `SigTable` 中移除 `group_exit_code` 及相关访问方法 |
| `os/src/task/process/process.rs` | `ProcessMeta` 增加进程私有 `group_exit_code`，并新增进程级退出状态 helper |
| `os/src/task/mod.rs` | 退出路径改用 `ProcessMeta` 保存/读取线程组退出码 |
| `os/src/signal/mod.rs` | 投递线程组信号时用 `Process::is_group_exiting()` 判断目标状态 |
| `os/src/syscall/task/wait.rs` | `waitpid()/waitid()` 改从 child 进程状态读取退出码 |

## 验证

已执行：

```text
make
```

结果：当前默认 LoongArch64 构建通过。

维护者最新 `log.ans` 显示 `clone02` 已不再 panic，musl/glibc 两轮 LTP Summary 均通过：

```text
RUN LTP CASE clone02
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0

RUN GLIBC LTP SINGLE CASE clone02
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

日志中仍有包装层输出 `FAIL LTP CASE clone02 : 12` /
`RESULT GLIBC LTP SINGLE CASE clone02 : 12`，但按项目规则以
`TPASS/TFAIL/TBROK` 和 Summary 为准；本次 Summary 的 `failed` 与 `broken` 均为 0。

未运行 RISC-V。
