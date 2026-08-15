# Loongson 2K1000 板级启动、AHCI 与 GMAC0 驱动

## 背景

Ya2yOS 的 LoongArch 路径原来只面向 QEMU `virt`：串口、关机地址、链接地址和根块设备均使用
QEMU PCI VirtIO 约定。Linux 7.0 已提供 Loongson 2K1000 Reference Board 的 LoongArch DTS，
对应 `loongson,ls2k1000-ref`，不能复用 QEMU 的设备地址和 VirtIO transport。

## Linux 证据

- `arch/loongarch/boot/dts/loongson-2k1000-ref.dts`：两个 LA264 CPU，EFI/FDT 启动，
  `stdout-path = "serial0:115200n8"`，高端 RAM 从 `0x90000000` 开始；
- `arch/loongarch/boot/dts/loongson-2k1000.dtsi`：UART0 为
  `ns16550a @ 0x1fe20000`，PMC poweroff 为 `0x1fe27000 + 0x14` 写 `0x3c00`，
  AHCI 控制器位于 `0x400e0000`；
- `drivers/ata/ahci_platform.c` 与 AHCI 规范路径说明控制器通过命令列表、received FIS、
  command table 和 PRDT 进行 DMA。本实现采用轮询，不依赖尚未接入的 LIOINTC 外设中断。
- `drivers/net/ethernet/stmicro/stmmac/dwmac-loongson.c`：GMAC 使用 DWMAC 3.x 正常描述符、
  RGMII 和 MDIO；其定制 DMA 中断状态只影响 Linux 的 IRQ/NAPI 路径。
- 本机 `LS2K1000-DP-V10` 固件 FDT：两路 `snps,dwmac-3.70a` 分别在 `0x40040000`、
  `0x40050000`，PHY 均为地址 0。本轮使用第一路实际 MMIO 地址，不猜测内部 PCI BAR。

## 根因

原 LoongArch 代码把 QEMU UART `0x1fe001e0`、关机寄存器 `0x100e001c` 和 PCI VirtIO 根盘硬编码
进平台路径。2K1000 的 SATA 是固定 MMIO AHCI，若继续初始化 QEMU PCI VirtIO，启动期会访问错误
的 PCI 配置空间并无法挂载根 EXT4。

## 修复

- 新增 `2k1000` feature 和 `board_2k1000` 平台。启动汇编保留 LoongArch EFI system-table
  参数，复用 FDT RAM/CPU 探测；链接入口为 `0x9000000090000000`，覆盖板上高端 DRAM 加载约定。
- console 使用 DTS 的 8-bit NS16550A UART 地址；`shutdown()` 改写 PMC poweroff 寄存器；次核启动
  限定为 2K1000 的两个 LA264 核。
- 新增单端口、单槽、轮询 AHCI 块驱动并让 `BlockDeviceImpl` 在该 feature 下选择它。驱动完成 HBA
  reset/AHCI enable、PI/SSS port-0 回退、COMRESET、IDENTIFY、LBA28/LBA48 DMA 读写与 cache flush。
  命令和数据 DMA 静态区位于内核直映射的低 4GiB 物理地址，符合该控制器 32-bit DMA 约束。
- `PLATFORM=2k1000` 使用专用 linker 配置，生成 `kernel-la` ELF 和 `kernel-la.bin` 原始镜像；
  `make run` 不启动不匹配的 QEMU，而是打印 U-Boot 加载命令。
- 新增 GMAC0 单通道轮询驱动。描述符和数据页由 CMA 分配，保守限制为 32-bit 物理地址；驱动
  通过 MDIO 先探测 DTS 指定的 PHY 0，再扫描兼容板修订，并从 RGMII 状态寄存器刷新链路速率。
  MAC/DMA 中断在轮询模式下保持屏蔽，避免依赖尚未接入的 LIOINTC。

## 上板

```bash
make build-arch TARGET_ARCH=loongarch64 PLATFORM=2k1000
```

将 `kernel-la.bin` 放到 TFTP 根目录后，在 U-Boot 执行：

```console
tftpboot 0x9000000090000000 kernel-la.bin
go 0x9000000090000000
```

## 验证

- `make build-arch TARGET_ARCH=loongarch64 PLATFORM=2k1000`：通过；
- `rust-readobj --file-headers --program-headers kernel-la`：entry 和首个 `PT_LOAD` 都是
  `0x9000000090000000`；
- `rust-nm -n kernel-la`：`AHCI_DMA_MEMORY = 0x9000000090568000`，对应物理
  `0x90568000`，低于 4GiB；
- `make build-arch TARGET_ARCH=loongarch64 PLATFORM=qemu`：通过；
- `make build-arch TARGET_ARCH=riscv64`：通过；
- 新增 GMAC0 后再次执行 `make build-arch TARGET_ARCH=loongarch64 PLATFORM=2k1000`：通过；
- `rust-nm -n kernel-la`：包含 `ls2k1000_gmac::Ls2k1000Gmac` 的初始化、收发路径符号；
- 未执行实体板 SATA/EXT4 读写、LTP 或网络回归。本机没有 2K1000 实板或可验证该 AHCI/LIOINTC
  路径的 QEMU 模型。

## 边界

GMAC1、SDIO、NAND、RTC 和 LIOINTC 外设中断尚未接入。GMAC0 也尚未进行实板 DMA reset、PHY、
ARP/DHCP 或 TCP/UDP 验证；AHCI 目前为同步轮询实现，需在接有 SATA 盘的实体板上验证 COMRESET、
IDENTIFY、EXT4 挂载及持续读写后，才能宣称完整实板可用。
