# VisionFive 2 JH7110 DWMAC 轮询网卡驱动

## 背景

VisionFive 2 板级启动和 UART 已可用，但 `visionfive2` 特性仍沿用 QEMU `virt` 的
VirtIO-net 地址 `0x10008000`，实板的板载 JH7110 GMAC1 (`ethernet@16040000`) 没有驱动。
RocketOS 含有早期 StarFive 网卡代码，但它使用了与当前 Ya2yOS 不同的网络接口和分配模型。

## 现象

VF2 内核即使开启 `net` 也不会发现板载网卡。直接移植 RocketOS 实现还会让 DMA 直接引用
普通 `NetBufPool` 或临时分配的内存；后者不保证物理连续，且发送完成索引和 RX 数据生命
周期无法和当前 `NetDriverOps` 对齐。

## 分析

Linux 7.0 将 VF2 的 GMAC1 定义为 `starfive,jh7110-dwmac` + `snps,dwmac-5.20`：

- `drivers/net/ethernet/stmicro/stmmac/dwmac-starfive.c` 通过 CRG 和 syscon 选择 RGMII，
  并把控制器交给通用 `stmmac`。
- `drivers/net/ethernet/stmicro/stmmac/dwmac4_descs.c` 定义 DWMAC4/5 的 16 字节描述符、
  OWN/FD/LD 位和 RX 长度；`stmmac_main.c` 在上交协议栈前去除 4 字节 FCS。
- `drivers/net/ethernet/stmicro/stmmac/stmmac_main.c` 将 N 项描述符环写成 `N - 1` 到
  ring-length 寄存器。
- `drivers/net/phy/motorcomm.c` 识别板载 YT8531 (`0x4f51e91b`)；VF2 DTS 为 GMAC1 使用
  RGMII-ID，并在速率变化时调整 TX 时钟极性。

Ya2yOS 目前没有 Linux 的 DMA 映射、cache sync、phylink、NAPI 或 GMAC IRQ 框架，所以不
复制 Linux 的异步框架。实现保留 Linux 的硬件寄存器/描述符语义，采用单队列、有界
MDIO/DMA reset 等待和协议栈调用点轮询。

## 修复

- 新增 `os/src/arch/riscv64/drivers/visionfive2.rs`，实现 JH7110 GMAC1 单队列 DWMAC
  5.20 驱动并实现现有 `NetDriverOps`。
- 在 `visionfive2` 特性下，RISC-V 的 `NetDeviceImpl` 改为该 GMAC；默认 QEMU RISC-V
  仍使用 VirtIO-net。
- 使用 CMA 分配描述符页和每个 RX/TX 描述符独占的 DMA 页。TX 从 `NetBuf` 复制到 DMA
  页后回收原缓冲区；RX 从 DMA 页复制到 `NetBufPool` 后立即把描述符归还硬件，避免普通
  内核堆物理不连续或硬件持有已释放网络缓冲区。
- 根据 Linux 的 VF2 DTS 启用 GMAC1 clocks/resets、syscon RGMII 选择、MAC/MTL/DMA 配置、
  MDIO PHY 扫描和 YT8531 的链路速率/双工设置。
- 仅启用轮询，保留 IRQ 78 作为设备元数据，不在尚未建立的中断分发路径中注册它。

## 验证

- `cargo fmt --manifest-path os/Cargo.toml -- --check`：通过。
- `git diff --check`：通过。
- `make build-arch TARGET_ARCH=riscv64 PLATFORM=visionfive2`：通过；RISC-V 内核特性为
  `warn,scheduler-cfs,visionfive2`，并成功生成板级镜像。

当前环境没有 VisionFive 2 实板，也没有可模拟 JH7110 GMAC 的 QEMU machine，因此没有宣称
链路协商、DHCP、ARP、TCP/UDP 或 IRQ 路径的运行通过。实板应先通过串口检查
`VisionFive2 GMAC1 initialized with PHY ...`，再使用静态 IP 执行 ARP/ping 和现有网络回归；
随后再接入 DMA cache sync 与 IRQ/NAPI 等性能路径。
