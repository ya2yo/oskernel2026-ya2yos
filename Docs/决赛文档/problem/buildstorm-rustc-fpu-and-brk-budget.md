# BuildStorm rustc 浮点上下文与 brk 预算

## 背景

BuildStorm 会并发运行 rustc 及其 linker-stage worker。RISC-V 用户态 Rust 程序使用 F/D 扩展，
同时 worker 的进程堆可能超过原有 512 MiB 的 `brk` 上限；LoongArch64 也需要与相同的 Rust 工具链峰值保持一致。

## 现象

暂存区注释记录了 RISC-V BuildStorm 中 `apfloat` 浮点常量解析出现 NaN 断言失败；旧的双架构 `USER_HEAP_SIZE`/
`MAX_BRK_SIZE` 仅为 512 MiB，linker-stage worker 的正常峰值可能触发 `brk` 增长失败。当前工作区未附带新的完整
BuildStorm 日志，因此不把这些改动描述为已经通过全量编译。

## 分析

RISC-V `trap_init()` 已将 `sstatus.FS` 置为可用，但 trap 汇编中的 `ENABLE_FPU` 仍为 0，用户态 trap 不保存/恢复
32 个 FPR 和 `fcsr`。任务切换后，后续任务可能继承前一任务的浮点状态，导致 rustc 的浮点计算结果被污染。

另一方面，`USER_HEAP_SIZE` 是 brk 区间的虚拟预留，`MAX_BRK_SIZE` 是每进程增长检查；两者扩大不会预先分配物理页，实际页
仍由缺页路径按需建立。原 512 MiB 限制与 Rust 工具链的工作集不匹配，会把可恢复的虚拟地址需求误报为 `ENOMEM`。

## 修复

- 在 `os/src/arch/riscv64/qemu/asms/trap.S` 将 `ENABLE_FPU` 设为 1，使用户态 trap 保存并恢复完整 FP 上下文。
- 将 RISC-V 和 LoongArch64 的 `USER_HEAP_SIZE`、`MAX_BRK_SIZE` 从 512 MiB 提升到 2 GiB；注释明确这是 lazy allocation
  下的虚拟预算，不等同于物理内存预留。

## 涉及文件

- `os/src/arch/riscv64/qemu/asms/trap.S`
- `os/src/arch/riscv64/qemu/memory_layout.rs`
- `os/src/arch/loongarch64/qemu/memory_layout.rs`

## 验证

本轮只根据暂存区补写文档并执行差异检查，未重新运行 RISC-V/LoongArch64 构建、QEMU、BuildStorm 或浮点定向测试。
后续应至少验证并发 rustc worker 的浮点结果、`brk` 超过 512 MiB 后的按需缺页，以及两架构完整 BuildStorm 的结束标记。
