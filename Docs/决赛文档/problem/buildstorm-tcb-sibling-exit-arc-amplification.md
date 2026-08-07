# BuildStorm 多 Hart 线程组退出时的 TCB 强引用放大

## 背景

BuildStorm 的多个线程会在同一线程组收到 `SIGKILL` 后并发退出。退出路径会检查兄弟线程状态、唤醒 futex 等待者，并在最后一个线程退出时回收进程资源。

## 现象

`tmp_01.ans` 中 `exit_current_and_run_next()` 报告多个 TCB 的 `strong_count = 4/5`，且出现时机随 Hart 调度顺序变化。典型线程组包括 PID 8、18 和 28。每个告警后都能找到相应的 `TCB N dropped`，所以这些计数不是永久保留的 TCB 泄露。

## 分析

`ProcessMeta::tasks` 本身保存的是 `Weak<TaskControlBlock>`。旧退出代码在两个位置都将整个快照一次性升级为 `Vec<Arc<TaskControlBlock>>`：

1. 唤醒被阻塞的兄弟线程。
2. 判断所有兄弟是否为 zombie 并累加 `ProcessUsage`。

多个 Hart 同时执行退出路径时，每个 Hart 都会暂时持有一份完整的兄弟 `Arc` 集合。一个 TCB 的额外强引用数量取决于并发退出的交错顺序，因此表现为时序相关的 `4/5` 峰值。调度器的 CFS 取队列仍在队列锁内把 `Ready` 预留为 `Running`，并结合 `on_cpu` 阻止稳定的重复调度；日志没有提供“同一 task 被两个 Hart 长期运行”的证据。

## 根因

退出路径把本应是非拥有的线程组快照扩展成了跨整个 teardown 过程存活的拥有引用集合。并发退出时，这会放大临时 `Arc` 计数，并延迟 TCB 的析构时刻。

## 修复

修改 `os/src/task/mod.rs`：

- 兄弟唤醒遍历保留 `Vec<Weak<TaskControlBlock>>`，逐项 `upgrade()`，处理并入队后立即释放该项 `Arc`。
- zombie 检查和用量统计同样基于 `Weak` 快照，单项升级只覆盖一次状态或时间数据读取，不再保存 `Vec<Arc>`。
- 保留原有 `ProcessMeta -> TaskControlBlockInner` 锁序和 futex 队列清理逻辑。

已有的 `strong_count` 警告和 `TCB dropped` 日志足以验证生命周期，未增加高频全路径引用计数 debug，避免再次放大 BuildStorm 日志。

## 验证

- `rustfmt --edition 2021 --check --config skip_children=true os/src/task/mod.rs`：通过。
- `git diff --check`：通过。
- `make TARGET_ARCH=riscv64`：通过，包含 RISC-V 与 LoongArch64 release 构建。
- `make log TARGET_ARCH=riscv64`：通过 debug 构建。
- `timeout 120s make run TARGET_ARCH=riscv64`：实际进入 BuildStorm 并触发 PID 28 多 Hart 退出；观察到兄弟 TCB 逐个 dropped。因来宾继续编译完整 Rust 依赖，外层超时以 124 结束，未完成 446/446，不能据此报告完整 BuildStorm 通过。
