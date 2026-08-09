# LoongArch QEMU TCG idle IPI 唤醒语义

## 背景

`os/src/arch/loongarch64/qemu/cpu.rs` 曾保留所有 Hart 的有界 polling，理由是
QEMU LoongArch 的 `idle 0` 不能可靠地被 scheduler IPI 唤醒。该判断导致空闲
Hart 持续消耗宿主 CPU，也把历史性能统计的现象直接归因于 QEMU TCG。

## 现象

历史 CAgent 和 BuildStorm 日志中，部分已启动 Hart 没有重新参与 CFS selection。此前以此
现象为依据，将 polling cohort 从 8 个扩展为全部 `HART_NUM` 个 Hart。

## 分析

检查当前工作树的 QEMU 源码可得到 TCG 下的完整路径：

1. `target/loongarch/insns.decode` 将 `idle` 解码为特权指令，
   `target/loongarch/tcg/insn_trans/trans_privileged.c.inc` 将下一 PC 写入
   `cpu_pc` 后调用 `helper_idle()`。
2. `target/loongarch/tcg/op_helper.c` 中的 `helper_idle()` 设置
   `CPUState.halted = 1` 并抛出 `EXCP_HLT`；这表示等待，不是永久停止。
3. `hw/intc/loongson_ipi_common.c` 在 IPI status 与 enable 位相交时拉高中断线。
   `target/loongarch/cpu.c` 将其映射到 LoongArch IRQ 12、设置 `ESTAT.IS12` 并发出
   `CPU_INTERRUPT_HARD`。
4. `accel/tcg/tcg-accel-ops.c` 和 `system/cpus.c` 会 kick 目标 vCPU 线程。
   `accel/tcg/cpu-exec.c` 随后清除 `halted`；`target/loongarch/tcg/tcg_cpu.c`
   在 `CRMD.IE` 与 `ECFG.LIE` 允许时进入中断入口。

内核已经满足这条路径的必要条件：`init_csr_regs()` 在每个 Hart 启用 IOCSR IPI vectors，
`enable_timer_interrupt()` 设置 `ECFG.LIE` 的 IPI 位，`idle()` 执行前设置 `CRMD.IE`，而
IPI trap 会清除 IOCSR 状态。仓库的 `scripts/loongarch64.mk` 也没有请求 KVM accelerator，
默认测试路径为 QEMU TCG。

## 根因

此前根据调度统计推断 QEMU TCG `idle 0` 无法由 IPI 唤醒，缺少对 QEMU 中断模型的源码验证。
该推断与当前 TCG 实现不符；历史 Hart 分布应另行从 IPI 发送、目标 CPU ID、IPI vector/en 位、
`ECFG.LIE12`、`CRMD.IE` 及调度器入队时机等内核状态排查，不能继续以 polling 作为 QEMU TCG
语义的替代。

KVM 是独立边界：QEMU 在 LoongArch IRQ 注入中调用 `KVM_INTERRUPT`，但 `idle` 的执行和唤醒
语义由宿主 Linux KVM 与硬件决定，不能只由 QEMU TCG 源码保证。

## 修复

删除 `POLLING_HARTS` 及其自旋分支。所有 LoongArch QEMU Hart 都在本地中断启用后执行
`idle 0`，并在 timer 或 scheduler IPI 返回后关闭 `CRMD.IE`、继续 remote TLB polling 和
调度循环。函数注释明确限定为 QEMU TCG 的已使能中断唤醒语义，不对 KVM 或实际硬件作保证。

## 涉及文件

- `os/src/arch/loongarch64/qemu/cpu.rs`
- `Docs/决赛文档/problem/loongarch-qemu-tcg-idle-ipi.md`
- `Docs/决赛文档/problem/loongarch-cagent-idle-polling.md`
- `Docs/决赛文档/problem/loongarch-buildstorm-polling-cohort.md`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

已执行目标文件格式检查和 LoongArch64 release 构建。未执行 QEMU 运行：当前工作区存在维护者的
未跟踪 `disk.img`，根目录 `make run` 会删除并重建该路径。运行验证时应确认使用 `-accel tcg`，并
检查 scheduler IPI 后的目标 Hart 是否继续出现 CFS selection。
