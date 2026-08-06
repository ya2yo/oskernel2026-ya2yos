# All-hart CFS 共享就绪队列

## 背景

共享地址空间 SMP 已允许同一线程组的线程在不同 Hart 上执行，但原 CFS 仍为每个 Hart 维护独立的 ready queue。这样调度器的竞争范围仍被切成多个局部队列，不能体现共享地址空间线程的全局 `vruntime` 公平性。

## 现象

`client.ans` 的 GDB 现场中，部分 CPU 处于 idle 或没有持续执行用户任务。该现象不必然表示次核启动失败：per-hart CFS 只从当前 Hart 的队列取任务，如果可运行任务集中在其它队列、任务的 `scheduled_hart`/affinity 又限制了候选范围，空闲 Hart 会正常进入 idle/WFI。即使改为全局队列，当可运行任务数少于在线 Hart 数或 affinity mask 只允许少数 Hart 时，也不会保证所有 Hart 同时有用户任务。

## 分析

旧实现把任务按 `scheduled_hart` 放入 `READY_QUEUES[hartid]`，每个 Hart 只比较自己的最小 `vruntime`。共享地址空间线程虽然拥有相同的 `MemorySet`，却无法从其它 Hart 的队列竞争；唤醒也只通知任务原先归属的 Hart。于是 GDB 看到的“未运行 CPU”可能只是局部队列没有可执行任务，而不是 CPU 没有上线。

all-hart CFS 需要同时处理三个并发条件：

1. 所有 Hart 必须从同一个 `(vruntime, tid)` 最小堆选择任务，且运行时间记账使用同一个 `min_vruntime` 基准。
2. 共享队列中的任务仍需按线程 `cpu_affinity` 过滤，不能为了全局公平把任务派发到禁止的 Hart。
3. 入队时要唤醒允许运行该任务的 idle Hart，但不能为每个入队事件向所有 Hart 广播 IPI。

## 根因

调度策略层仍保留 per-hart ready queue 和 `scheduled_hart` 队列归属，线程级 placement 因而退化为队列级固定 placement。`scheduled_hart` 适合表示最近一次实际运行 Hart，却不应继续作为 CFS 队列所有权。

## 修复

- `os/src/task/scheduler/cfs.rs` 将多个 `READY_QUEUES` 合并为一个带锁的 `READY_QUEUE`，所有 Hart 共享最小堆和 `min_vruntime`；取任务时保留不满足当前 affinity 的堆项，继续寻找当前 Hart 可运行的最小 `vruntime` 任务。
- `os/src/task/scheduler/mod.rs` 让 CFS 入队返回共享队列长度，并根据任务 affinity 唤醒有限数量的 idle Hart；RR 分支继续使用原有 per-hart placement。
- `os/src/task/processor.rs` 增加按 queued task 数量限制的 idle Hart 唤醒逻辑。任务真正被选中后才更新 `scheduled_hart`，避免把“最近运行 Hart”误当成共享 CFS 队列归属。
- `os/src/task/task/task.rs` 增加 `can_run_on()` 和 `set_scheduled_hart()`，将 affinity 判断与实际执行 Hart 发布分开。
- `os/src/syscall/task/schedule.rs` 在 CFS affinity 更新后唤醒新 mask 中的 idle Hart；远程运行任务仍通过旧 Hart 的 IPI 到达迁移边界，RR 分支保留原 placement 通知。
- `os/src/task/mod.rs` 更新抢占和让出路径注释，使其表达“当前 Hart 有可运行候选”而非“本地 home Hart 独占”。

共享 CFS 使用一个 `spin::Mutex` 保护堆结构，先保证调度公平性和语义正确性；若后续 perf 结果显示锁竞争成为瓶颈，再考虑分层队列或候选缓存。affinity 更新与并发取任务之间仍存在极窄的竞争窗口，当前由取任务前后 affinity 检查、迁移通知和下一调度边界收敛，后续应继续审计。

## 涉及文件

- `os/src/task/scheduler/cfs.rs`
- `os/src/task/scheduler/mod.rs`
- `os/src/task/processor.rs`
- `os/src/task/task/task.rs`
- `os/src/syscall/task/schedule.rs`
- `os/src/task/mod.rs`

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- `make build-arch TARGET_ARCH=riscv64 SCHEDULER=rr`：通过，确认 RR 编译期分支未被共享 CFS 改动破坏。
- `make perf TARGET_ARCH=riscv64`：通过。
- 目标文件 `rustfmt --edition 2021` 与 `git diff --check`：通过。
- RISC-V `/tmp` qcow2 overlay 运行：QEMU 识别 8 个 Hart，8 个 Hart 均进入内核启动路径；`sigaltstack regression: PASS`、`rseq regression: PASS`，并进入 `OS COMP TEST GROUP START buildstorm`，无 panic、`TFAIL` 或 `TBROK`。样本在完整 BuildStorm 结束前超时，因此没有端到端吞吐或性能提升百分比结论。
- 普通 `make run` 未执行成功，原因是维护者已有 QEMU 实例持有正式 `disk.img` 写锁，返回 `Failed to get "write" lock`；未终止该实例或覆盖正式镜像。
