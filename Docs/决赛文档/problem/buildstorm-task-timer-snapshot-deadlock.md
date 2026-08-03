# BuildStorm 定时扫描任务表快照与内核堆锁死锁

## 背景

`check_blocked_task_timers()` 在每个 hart 的调度循环中补扫处于 `Blocked` 状态的任务，保证阻塞在
`accept`、`recv` 等内核路径的任务仍能收到 `ITIMER_REAL` 到期信号。为减少扫描过程中反复获取全局任务表，曾改为先按
hart 收集一个 `Vec<Arc<TaskControlBlock>>` 快照，再逐项检查。

## 现象

`server.ans` 已通过 `BUILDSTORM_TOOLCHAIN` 和 `BUILDSTORM_MINIBUILD`，Cargo 最后可见进度为
`Building 46/446`（完成 `socket2 v0.6.4`），此后没有新的编译输出。

`client.ans` 的全 CPU GDB 栈显示：

- CPU#6 在 `get_tasks_on_hart()` 的 `Vec::collect()` 扩容中，等待全局 `HEAP` ticket mutex；
- CPU#0 同样执行 `get_tasks_on_hart()`，等待 `TID_TO_TASK` spin mutex；
- CPU#1 正在处理 `SIGKILL` 线程组退出，销毁 `MemorySet` 的页帧引用时等待 `HEAP`；
- 其余 hart 位于 `idle_until_runnable() -> wfi`。

因此停滞不是 Cargo 的慢编译阶段，也不是 lwext4 资源锁现场；调度器中的全局任务表与内核堆锁已经形成无进展等待。

## 分析

原实现为：

```rust
TID_TO_TASK
    .lock()
    .values()
    .filter(|task| task.process.home_hart() == hartid)
    .cloned()
    .collect()
```

链式表达式中 `TID_TO_TASK.lock()` 返回的 guard 会活到整个 `collect()` 结束。`Vec` 首次扩容或增长时进入全局分配器，
所以实际锁顺序是 `TID_TO_TASK -> HEAP`，并非原注释声称的“释放任务表锁后再处理”。同时 `filter` 还在任务表锁内读取
`process.home_hart()`，扩大了全局锁保护区。

GDB 已直接验证 CPU#6 的 `RawVec::grow_amortized` 位于该 guard 生命周期内；CPU#0 的任务表等待和 CPU#1 的页帧回收堆等待
说明该组合会阻塞定时维护及任务退出收敛。

## 根因

为降低全局任务表获取次数而引入的快照分配违反了锁边界：在 `TID_TO_TASK` guard 存活期间调用可能取得 `HEAP` 的
`Vec::collect()`。这是 `b93ad36a` 的性能改动引入的回归。

## 修复

恢复无分配的 `for_each_task()` 遍历：每轮仅在 `TID_TO_TASK` 锁内从 `BTreeMap::range()` 取下一项并 `Arc::clone`，
随后立即释放任务表锁。hart 归属判断、PCB 状态读取和 `deliver_blocked_itimer_signal()` 均在锁外执行。

该方案不在全局任务表锁内分配，也不在其内进入任务、进程或信号锁。代价是每个候选任务短暂获取一次任务表锁；它避免了不可恢复的锁依赖，正确性优先于这条微优化。

## 涉及文件

- `os/src/task/manager.rs`
- `server.ans`
- `client.ans`

## 验证

- `cargo fmt --manifest-path os/Cargo.toml --all -- --check` 通过。
- `git diff --check` 通过。
- `make perf TARGET_ARCH=riscv64` 已启动用户态与内核构建，但 lwext4 CMake 找不到宿主 `riscv64-linux-musl-cc`，因此未完成链接；失败发生在本机交叉工具链缺失，非本修复的 Rust 编译诊断。
- 尚未在带 musl 工具链的 Docker 环境重跑 BuildStorm，故不能宣称 `46/446` 后已完成或本死锁已得到运行时复现验证。
