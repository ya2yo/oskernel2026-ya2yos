# LoongArch CAgent idle hart 唤醒与 CFS polling cohort

> 2026-08-09 勘误：本复盘保留当时的性能观测，但其中“QEMU TCG 的 `idle 0`
> 不能可靠由 scheduler IPI 唤醒”的根因结论不成立。当前 QEMU TCG 源码在
> `idle 0` 后会将 vCPU 置为 halted，已使能的 pending IPI 会 kick vCPU、清除
> halted 并进入中断入口。相关轮询绕过已移除；详见
> [LoongArch QEMU TCG idle IPI 唤醒语义](./loongarch-qemu-tcg-idle-ipi.md)。

## 背景

`final-2026/sdcard-la.img` 的 CAgent 会同时启动 cpu、文件系统、网络和 kernel
共十个任务。原始 `log.ans` 在 `-smp 12` 的 LoongArch64 QEMU 配置下全部通过，但每项
用例都耗时约 3.9--5.4 秒，端到端 guest 运行窗口约为 10.5 秒。

| 用例 | 原始日志耗时（ms） |
| --- | ---: |
| cpu | 4821 |
| fs-create | 3864 |
| factorial | 4334 |
| date | 4402 |
| network | 5031 |
| fs-readwrite | 5157 |
| fs-search | 4641 |
| kernel | 5204 |
| fs-usage | 5181 |
| fs-directory | 5384 |

## 现象

原始 perf 快照中，`pipe read wait`、`read`、`recv`、`accept` 和 `vfork` 都有数秒级
累计等待。这些指标覆盖并发任务，不能直接相加或当作单一热点；真正异常在于调度分布：

```text
scheduler_harts selections_by_hart=[0, 0, 21034, 0, 0, 0, 0, 0, 0, 0, 0, 0]
idle_loops_by_hart=[1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1]
interval_scheduler_wakeup ... remote_ipi_sent=21033 remote_ipi_failed=0
```

十二个 hart 均已启动，且调度器确实向远端发送 IPI，但约两万次 task selection 全部落在
hart 2；其余 hart 只进入过一次 idle。因而 bash 子进程、HTTP 服务和文件系统任务几乎都
在一个 hart 上串行执行。

## 分析

将 LoongArch idle 指令改为 `idle 0` 的对照运行没有改变上述单 hart 分布，说明问题不在
idle level。本内核用于唤醒的 scheduler IPI 在 QEMU LoongArch idle 状态下无法可靠返回到
调度循环；IPI 的送达统计并不意味着 idle hart 已重新检查共享 CFS 队列。

这不是 EXT4 全局锁主导的串行化：原始快照中 EXT4 block-device service 约 0.61 秒，资源
锁等待为 0；而在恢复多 hart 运行后，因真实并发而出现的块设备/namespace 锁等待反而增大。
因此不能以降低文件系统锁时间作为本问题的优化目标。

## 根因

LoongArch64 QEMU 的架构 idle 路径与当前 Ya2yOS scheduler IPI/trap 返回路径组合时，idle
hart 不会稳定地回到调度循环。共享 CFS 队列虽然已有可运行任务，远端 hart 仍保持停机状态，
导致 12-hart 配置退化为单 hart 调度。

根因的适用边界是当前 LoongArch QEMU/内核组合；没有将其外推为 LoongArch 硬件问题，也没有
修改 CFS 语义、任务 affinity 或 vendored `loongArch64` crate。

## 修复

在 `os/src/arch/loongarch64/qemu/cpu.rs` 中保留 `min(HART_NUM, 8)` 个 polling hart。它们在
每个 idle interval 执行有界的 4096 次 `spin_loop()` 后重新回到调度路径并轮询 remote TLB；
其余 hart 启用中断后执行 `idle 0`。这样至少八个 hart 不依赖不可靠的 idle IPI 返回，而仍让
四个剩余 hart 使用低功耗 idle，避免全 12-hart 忙轮询。

选择八个 hart 是吞吐与宿主 CPU 消耗的折中。该路径仅编译到 LoongArch QEMU 架构层，且没有
改变 wakeup、IPI、锁或用户可见 syscall 的语义。

## 对照结果

所有运行均使用同一 LoongArch64 final 镜像和 `-smp 12`，wall-clock 由宿主
`/usr/bin/time` 记录。`user_s`/`sys_s` 是多个 QEMU vCPU 线程的累计宿主 CPU 时间，不应与
wall-clock 相加。

| idle 策略 | wall-clock（s） | 宿主 user/sys（s） | 调度分布 | 结论 |
| --- | ---: | ---: | --- | --- |
| 原始全 idle | 11.37 | 未单独记录 | 实际仅一个 hart | 基线 |
| 4 个 polling hart | 5.84 | 29.56 / 18.74 | 4 个 hart | 并发不足 |
| 8 个 polling hart | 5.20、5.19 | 41.74 / 6.93；41.26 / 7.02 | 8 个 hart | 采用 |
| 12 个 polling hart | 4.55 | 46.07 / 1.25 | 12 个 hart | 宿主 CPU 代价过高 |

采用方案的重复样本中，最终调度器统计为：

```text
selections_by_hart=[4676, 5682, 5731, 5871, 6076, 4023, 6066, 6076, 0, 0, 0, 0]
```

原始 11.37 秒到 8-hart 的约 5.20 秒，端到端 wall-clock 降低约 54%。第二次样本为 5.19 秒，
确认该收益不依赖单次 QEMU 抖动。

## 验证

已执行：

```text
make TARGET_ARCH=riscv64
make TARGET_ARCH=loongarch64
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
/usr/bin/time -f 'elapsed_s=%e user_s=%U sys_s=%S' \
  timeout 180s make run TARGET_ARCH=loongarch64
/usr/bin/time -f 'elapsed_s=%e user_s=%U sys_s=%S' \
  timeout 180s make run TARGET_ARCH=riscv64
rustfmt --edition 2021 --check os/src/arch/loongarch64/qemu/cpu.rs
git diff --check
```

两架构 release/perf 构建均通过。LoongArch 8-hart 重复运行的十项 CAgent 全部 `pass` 并
`shutdown!`，用例时间为 855--2026 ms；RISC-V perf QEMU 也完成十项 CAgent 并 `shutdown!`，
用于确认跨架构没有回归，不用于 LoongArch 性能比较。全仓 `cargo fmt --check` 仍会报告本轮未
触及文件的既有格式差异；目标文件格式检查已通过。

完整 BuildStorm 和 LTP 长时运行未在本轮执行。该优化会提高 QEMU 宿主 CPU 占用，后续若修复
LoongArch idle IPI 返回路径，应删除 polling cohort 并重新比较 wall-clock 和 CPU 消耗。
