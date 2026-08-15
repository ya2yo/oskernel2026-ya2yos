# BuildStorm P1-A 当前任务重入队远程 IPI 风暴

## 背景

`optimize.txt` 根据 `loongarch.ans` 将当前任务重入队造成的远程 IPI 风暴列为
P1-A。基线全程记录 `remote_enqueues=1,279,469`、`remote_ipi_sent=1,203,347`，
最后约 23 秒的嵌套 QEMU 阶段又产生 `513,509` 次远程入队和 IPI。P0 要求拆分
BuildStorm-only 与 nested-QEMU-only，但当前评测机沿单一入口顺序执行全部阶段，
本次不改变测试入口，也不宣称完成 P0 拆分。

## 现象

调度器每次从当前 Hart 切出仍可运行的任务时，都会把它重新放入共享 CFS/RR 就绪
队列。该路径使用普通 `ready_queue::add_task()`，在队列已经由当前 Hart 驱动的
情况下仍可能检查并唤醒远程 idle Hart，造成无效 IPI。外部 wake-up、新任务发布和
affinity 失败重入队则必须保留远程通知，否则可能延迟可运行任务。

## 根因

`run_tasks()` 的当前任务 requeue 与阻塞任务从 `Blocked` 转为 `Ready` 共用了带通知
的入队 API。队列去重只避免重复队列项，不会抑制 `add_task()` 后续的 Hart 通知，因而
“任务已在 CPU 上运行过、只是调度记账后再次参与竞争”的 bookkeeping 操作被错误地
当作外部唤醒。

## 修复

- 在 `os/src/task/scheduler/mod.rs` 增加 `ready_queue::requeue_current()`，复用
  CFS/RR 策略的入队逻辑但不发送 Hart 通知。
- `os/src/task/processor.rs` 的当前任务重入队改用该专用 API；affinity 检查失败
  仍调用普通 `add_task()`。
- 在 `os/src/utils/perf/scheduler.rs` 增加 `current_requeues` 和
  `current_requeue_notifications` 聚合计数，并在周期/累计调度报告输出。后者只
  统计该路径实际发送的通知，优化后应为 0；普通外部唤醒继续由既有
  `remote_enqueues`/`remote_ipi_sent` 统计。

## 验证

已执行：

```text
make TARGET_ARCH=riscv64
make TARGET_ARCH=loongarch64
make perf TARGET_ARCH=riscv64
make TARGET_ARCH=riscv64 SCHEDULER=rr build-arch
git diff --check
```

RISC-V、LoongArch64 CFS release 构建和 RISC-V RR 构建均通过。带 perf 的 RISC-V
QEMU 运行 120 秒，日志 `/tmp/p1a-riscv-perf.log` 中观察到：

```text
current_requeues=80771 current_requeue_notifications=0
remote_enqueues=9421 remote_ipi_sent=8393
```

因此当前任务重入队路径确实没有发送通知，外部唤醒计数仍在增长。标准 RISC-V
QEMU 运行 180 秒已完成 cagent 并进入 `BUILDSTORM_TOOLCHAIN ok`，内核启动输出
`[kernel] Hello, world!`，但在 BuildStorm 预构建阶段超时，未到达嵌套 QEMU 的
`shutdown!`；该结果不能替代完整 nested-QEMU-only 验收。运行日志保存在
`/tmp/p1a-riscv-run-escalated.log`。

## 边界

本次只实施 P1-A，没有改变 sparse I/O、页缓存、TLB 或 timer 路径，也没有将 P0
的阶段拆分伪装成优化结果。完整 BuildStorm wall-clock、嵌套 QEMU 最终关机和
`remote_ipi_sent` 至少下降 90% 的长测仍需在评测机完整入口上复测。
