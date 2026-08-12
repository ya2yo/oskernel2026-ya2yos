# 启动器探测 RAM 布局

## 背景

`os/src/arch/{riscv64,loongarch64}/qemu/memory_layout.rs` 曾分别保存 QEMU RAM
起址、容量和 LoongArch PCI/MMIO 空洞两侧的固定 RAM 段。修改 QEMU `-m`、NUMA
配置或启动器生成的设备树后，页表和 CMA 可能继续使用旧布局。

## 现象

RISC-V 已从 SBI `a1` 收到 FDT，但 memory layout 文件中仍保留未同步的固定 RAM
常量。LoongArch QEMU `virt` 直接内核启动时不把 FDT 直接放入通用入口参数，旧代码因而
退回 `0x9_0000_0000` 及两个固定物理段，不能反映启动器实际发布的内存图。

## 分析

QEMU 9.2 LoongArch `virt` 的直接启动器在 `a2` 传递 EFI system table 的物理偏移。
该表的 Device Tree configuration table（GUID
`b1b621d5-f19c-41a5-830b-d9152c69aae0`）给出 FDT 地址。FDT 的每个
`memory@.../reg` 条目是实际可用 RAM 段；当 RAM 越过 PCI/MMIO hole 时，会自然给出多个段。

RISC-V `virt` 的 SBI 约定则直接在 `a1` 提供 FDT。两个架构都可以由同一个无堆 FDT
reader 解析根节点 cell 宽度、所有 RAM `reg` 元组及 CPU/timebase 信息。

## 根因

内核把启动器已经拥有的平台真值复制为架构源文件中的常量，并且 LoongArch 没有消费其 EFI
system table 中的 FDT。CMA 还直接遍历 LoongArch 固定段表，导致内存管理与启动器描述
脱节。

## 修复

- `arch::hardware` 以固定上限的原子段表保存所有经 FDT 验证的页对齐 RAM 段；解析无有效
  RAM 段即失败，不再悄悄采用旧容量。
- LoongArch 入口将 `a2` 传入 CSR 初始化，bootstrap hart 通过 EFI system table 定位 FDT；
  该启动参数缓存位于 `.data`，避免 `clear_bss()` 清除后使次核丢失它。
- 两份 `memory_layout.rs` 删除 RAM 起址、大小、`MEMORY_END` 和分段 RAM 常量。LoongArch
  内存布局调试输出改为实际探测的每个段。
- LoongArch CMA 直接遍历探测段；RISC-V 保持 bootstrap 页表只覆盖一个连续 RAM 段的约束，
  在 FDT 不满足该当前平台契约时明确断言。
- 删除 RISC-V 与 LoongArch64 架构目录中的 `config.rs` 和固定 `HART_NUM`。启动、IPI、
  调度、affinity、`/proc/cpuinfo` 与 perf 输出使用 FDT 探测的在线 Hart 数；静态数组和入口
  汇编栈改用不表示平台拓扑的 `MAX_SUPPORTED_HARTS = 16` 资源容量。

## 涉及文件

| 文件 | 修改 |
| --- | --- |
| `os/src/arch/hardware.rs` | 多段 FDT RAM 解析和 LoongArch EFI FDT 定位 |
| `os/src/arch/loongarch64/qemu/asms/entry.asm` | 向 Rust 传递 `a2` EFI system-table offset |
| `os/src/arch/loongarch64/qemu/cpu.rs` | bootstrap/次核共享启动器参数并进入统一 FDT 路径 |
| `os/src/arch/*/qemu/memory_layout.rs` | 删除 RAM 硬编码 |
| `os/src/mm/frame_alloc/buddy_cma.rs` | CMA 使用探测 RAM 段 |
| `os/src/main.rs` | 启动时强制要求有效 FDT RAM 描述 |
| `os/src/{task,mm,utils}/` 与架构 CPU 代码 | 消费运行时 Hart 数，静态资源使用容量上限 |

## 验证

- 本机 QEMU 9.2 的 `-machine virt,dumpdtb=/tmp/ya2yos-loongarch-virt.dtb -m 16G`
  导出确认存在两个 `memory@...` 节点，其 `reg` 正确表达低端 RAM 与 PCI/MMIO hole 之后的
  高端 RAM。
- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- `timeout 45s make run TARGET_ARCH=riscv64` 和 LoongArch64 均在 QEMU 创建
  `/var/tmp/vl.*` 临时文件前被当前沙箱的只读 `/var/tmp` 阻断，未进入内核；本轮没有 QEMU、
  LTP 或 BuildStorm 运行结论。
