# VisionFive 2 板级启动与 UART 驱动

## 背景

Ya2yOS 原有 RISC-V 入口、链接脚本、控制台和 MMIO 表针对 QEMU `virt`，无法直接用于
StarFive JH7110 VisionFive 2。RocketOS 参考实现确认该板由 U-Boot/OpenSBI 在物理
`0x40000000` 区域交接，内核高地址为 `0xffffffc040200000`。

## 现象

原构建只能生成 QEMU `kernel-rv`：启动页表只映射 `0x80000000`，链接地址是
`0xffffffc080200000`，控制台使用 SBI legacy console，板级串口和 JH7110 MMIO 未纳入
内核地址空间。

## 分析

VisionFive 2 的板级启动需要在打开 Sv39 前保留物理恒等映射，同时建立高地址别名；JH7110
UART0 是位于 `0x10000000` 的 NS16550 兼容控制器，可以通过 LSR 的 `DR/THRE` 位轮询。
现有 FDT RAM 探测、CMA 和高地址直接映射可以复用，不需要为板子重新实现内存分配器。

RocketOS 中的 GMAC/SDIO 实现还依赖独立的 JH7110 时钟、PHY、DMA 和 `dw_sd` 外部驱动；
这些依赖不在当前 Ya2yOS vendor 集合内，因此本次只接入启动、串口和板级 MMIO 基础层，
不宣称网卡或 SD 控制器已经可用。

## 修复

- 新增 `visionfive2` Cargo feature、`scripts/visionfive2.mk` 和板级 Cargo 配置。
- 新增 `visionfive2.ld`，把内核链接到 `0xffffffc040200000`。
- 新增 `entry_visionfive2.asm`，建立 `0x40000000` 恒等映射与
  `0xffffffc040000000` 高地址映射，并保留 `sigreturn_trampoline`。
- 新增 JH7110 NS16550 UART 驱动，RISC-V 控制台在该 feature 下不再调用 SBI legacy console。
- 增加 UART、GMAC、时钟/复位、syscon 和 GPIO 的板级 MMIO 映射。
- 保留 QEMU 默认链接脚本、SBI 控制台和 VirtIO 地址，两个平台通过 feature 隔离。

## 验证

- `make TARGET_ARCH=visionfive2 build-arch`：通过，用户态和内核均完成构建并生成 `kernel-vf2`。
- `rust-objdump -h os/target/riscv64gc-unknown-none-elf/release/os`：`.text` 起始为
  `0xffffffc040200000`。
- 默认 RISC-V 构建回归已发起；未在本记录中宣称 QEMU 运行或 LTP/BuildStorm 通过。
- 当前环境没有 VisionFive 2 实板或可用 QEMU JH7110 模拟器，因此未执行板上启动、GMAC
  网络和 SD 卡读写验证。
