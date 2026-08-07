# BuildStorm all-hart CFS 重复调度导致的 page fault panic

## 背景

all-hart CFS 使用一个由所有 Hart 竞争的共享就绪堆。Linux 将任务睡眠状态、
`on_rq` 和 `on_cpu` 分开维护：前者表达任务是否可运行，后两者分别表达任务是否
已进入 runqueue、旧 CPU 是否仍持有其执行上下文。三者必须在取任务、切换、唤醒
和重新入队之间保持一致，否则同一个 TCB 可能被多个 Hart 同时恢复。

## 现象

`server.ans` 在 `buildstorm` 阶段首先报告 `Cause: Exception(LoadPageFault)`，
`stval=0x2a`；随后多个 Hart 报告 `spin::once::Once panicked`。`client.ans` 的
回溯落在 `Arc<DetachedMountFd>::clone`、任务表访问、`mmap_write_page_fault`
和 `current_trap_cx`，并出现异常的当前任务指针。这些是调度同一 TCB 造成内核
栈、页表或共享对象状态损坏后的连锁故障，不是 `spin::Once` 初始化本身的首发
错误。

第一次修复 `on_rq` 预留窗口后，新的 `server.ans` 在 BuildStorm
`pre-build tg-xtask` 约 `440/446` 又出现 `FetchInstructionPageFault`：`stval=0x38`、
`sepc=0x38`，GDB 回溯从 `trap_from_kernel` 跳到 `0x38` 并提示栈损坏。另一 Hart
同时位于正常的 `MemorySet::handle_page_fault()`，说明仍有任务上下文被并发恢复，
而非 `0x38` 对应真实内核代码。

## 分析

旧的 `fetch_task` 从共享堆弹出条目后先清除 `on_rq`，但任务仍保持 `Ready`，
直到 `run_tasks` 取得返回值后才发布 `Running`。在这个窗口中，唤醒/重新入队
路径可以观察到“未入队且 Ready”的 TCB，创建第二个有效堆项；另一个 Hart
随后能够再次选中同一任务。共享地址空间下，这会并发使用同一个用户页表和任务
内核上下文，最终表现为随机 page fault 和后续 panic。

进一步审计阻塞路径发现另一个窗口：pipe、futex 和通用信号等待先将任务设为
`Blocked`，随后 `schedule_blocked_current()` 在 `switch()` 保存上下文之前就调用
`take_current_task()`。并发唤醒可在旧 Hart 仍执行该任务内核栈时把状态改为 `Ready`
并放入共享 CFS；另一个 Hart 随即恢复尚未保存完的同一 `TaskContext`。仅增加新的
任务中间状态会混淆 Linux 睡眠语义，也不能替代跨 CPU 的上下文所有权协议。

## 根因

根因包含两个独立但相关的调度属性缺口：

- 队列占用状态与任务运行状态不是同一个临界区内的状态机转换，曾出现
  `on_rq=false`、`TaskStatus::Ready` 和“已被某个 Hart 预留”同时可见。
- TCB 没有 Linux `task_struct::on_cpu` 的等价属性，唤醒方无法区分“已经睡眠”与
  “状态已改回 Ready，但旧 Hart 尚未完成上下文切出”。

## 修复

- `add_task` 先持有共享队列锁，再检查任务是否仍为 `Ready`，并在同一锁协议下
  执行 `on_rq` 去重，拒绝非就绪任务的 stale 入队。
- `fetch_task` 在共享队列锁和 TCB 锁保护下，将可执行任务从 `Ready` 预留为
  `Running`，然后清除 `on_rq`，使其他 Hart 看不到可重复选择的状态窗口。
- 取任务后若 affinity 已变化，调度循环先把预留状态恢复为 `Ready`，再重新入队。
- 在 TCB 增加独立原子 `on_cpu`，不新增任务状态。调度器在恢复任务上下文前置位，
  在 `switch()` 已返回 idle 栈、旧上下文完整保存后以 Release 顺序清零。
- CFS 和 RR 入队同时要求 `TaskStatus::Ready` 与 `on_cpu=false`。阻塞期间的并发唤醒
  可以按 Linux 语义直接改回 `Ready`，但入队延迟到旧 Hart 完成切出；源 Hart 与
  唤醒方同时补入队时继续由 `on_rq` CAS 合并，不再把合法竞争打印为 WARN。
- `block_current_and_run_next()`、`schedule_blocked_current()`、Future 等阻塞路径以及
  stop 路径不再在 `switch()` 前移除 `Processor::current`。pipe 读等待二次检查同时
  覆盖写端已全部关闭，避免关闭事件落在 waiter 登记前时永久睡眠。

## 涉及文件

- `os/src/task/scheduler/cfs.rs`
- `os/src/task/scheduler/rr.rs`
- `os/src/task/processor.rs`
- `os/src/task/task/task.rs`
- `os/src/task/mod.rs`
- `os/src/task/future/mod.rs`
- `os/src/fs/files/pipe/file_impl.rs`
- `os/src/task/futex.rs`（等待协议注释）

## 验证

- `make TARGET_ARCH=riscv64`：通过；默认构建流程同时完成 LoongArch64 构建。
- `git diff --check`：通过。
- 使用 `/tmp/ya2yos-cfs-fix.qcow2` 隔离 overlay 运行 RISC-V QEMU 240 秒：8 个
  Hart 均启动，`sigaltstack regression: PASS`、`rseq regression: PASS`，进入
  `buildstorm` 并输出 `BUILDSTORM_TOOLCHAIN ok`；日志无 `panic`、`LoadPageFault`、
  `StorePageFault`、`TFAIL` 或 `TBROK`。测试在完整 BuildStorm 结束前因超时停止，
  未据此宣称 446/446 完整通过。
- 正式 `make run` 未使用，因为维护者已有 QEMU 进程持有正式镜像写锁；未终止该
  进程，也未修改正式镜像。

后续维护者提供的 RISC-V `fault-diagnostics,perf` 日志已经从原故障点
`440/446` 推进到 `444/446: axbuild`，没有新的 panic 或损坏回溯。累计
`selections_by_hart=[1681, 2008, 1067, 975, 838, 717, 589, 510]`，证明 8 个 Hart
均参与过调度；GDB 在 `axbuild` 阶段只看到一个 Hart 执行 `mmap/mprotect`，是因为
非计时的 `cargo build -p tg-xtask` 已到依赖图尾部，只剩一个 Cargo 编译单元，
不是 affinity 或 CFS 单核运行。日志停在 `444/446`，仍不宣称完整 BuildStorm 通过。

当前代码另完成以下构建检查：

- `make TARGET_ARCH=riscv64`：RISC-V 与 LoongArch64 CFS release 构建通过。
- `make build-arch TARGET_ARCH=riscv64 SCHEDULER=rr`：RISC-V RR 构建通过。
- `git diff --check`：通过。

Linux 对照证据来自 `include/linux/sched.h` 的 `TASK_RUNNING`/睡眠态说明，以及
`kernel/sched/core.c` 的 `try_to_wake_up()`、`prepare_task()`、`finish_task()`：
Linux 同样不引入“Blocking”任务状态，而是用 `on_cpu` 的 release/acquire 交接保证
旧 CPU 完成切出后才允许唤醒任务迁移并重新入队。
