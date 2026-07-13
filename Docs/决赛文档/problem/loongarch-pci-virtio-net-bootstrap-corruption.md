# LoongArch PCI VirtIO-net early boot corruption

## 背景

LoongArch64 QEMU 使用 PCI 总线提供 VirtIO block 与 VirtIO network 设备。启用
`DeviceContainer::from_one(NetDeviceImpl::new_device())` 后，内核会在调度器建立
每任务内核栈之前完成 PCI 枚举、VirtQueue 建立、RX buffer 投递和网络子系统注册。

## 现象

原始 `log.ans` 在以下输出后停止：

```text
net::init...Found VirtIO network device at bus=0, device=2, func=0
```

早期调试还观察到互相看似无关的表现：UART 基址变为零、`Lazy instance has previously
been poisoned`，以及 initproc 读取 `/initproc` 时 `lwext4_rust::file::CACHE_TABLE` 路径
发生异常。这些表现会随日志和调用深度变化，说明存在启动期内存破坏。

## 分析

### 启动栈越界

LoongArch 的 `entry.asm` 原先只为每个 hart 预留 4 KiB 启动栈。当前 release ELF 的
反汇编显示：

```text
rust_main          stack frame: about 0x7c60 bytes
net::init_network  stack frame: about 0x5c60 bytes
```

两者嵌套时已远超 4 KiB。更重要的是，`boot_stack` 紧随 `.data`：其下方约 0xc70 字节
即为 `lwext4_rust::file::CACHE_TABLE`，其中还包含 UART 等静态对象。网卡实例包含两个
128-entry VirtQueue、buffer slot 数组和内联 `SmallVec`，实际初始化会写入此前只被编译器
保留的栈帧，因此启用网卡后稳定覆盖相邻 `.data`。

先将栈提升到 64 KiB 后，PCI 和 VirtIO 初始化可以继续，但网络子系统、文件缓存和
initproc 路径仍可能使栈深超过余量。提升到 256 KiB/ hart 后，启动能够稳定完成，证明
`CACHE_TABLE` 异常是同一栈越界的后果，而不是 ext4 或 RX DMA 的独立损坏。

### PCI 配置空间访问

本地 PCI helper 曾把任意 capability byte offset 直接转换为 `*mut u32` 后普通读写。能力
链扫描会读取 `offset + 1`、`offset + 3` 等非 4 字节对齐位置，这在 Rust 中构成未对齐
解引用和非 volatile MMIO 访问的未定义行为。原 capability 调试输出还把 `cap_len` 当成
`cfg_type`，并把 `cfg_type` 当成 BAR 索引。

### VirtQueue DMA 初始化

`virtio_drivers::Hal::dma_alloc()` 要求返回的连续 DMA 页面已经清零。CMA 分配器可返回
先前使用过的页面，旧实现没有清零 queue descriptor/avail/used ring；设备可能把残留 index
解释为已提交请求，在驱动真正投递 RX buffer 前读取错误 descriptor。

## 根因

主故障是 4 KiB 的 LoongArch 早期启动栈无法容纳启用 PCI VirtIO-net 后的 Rust 调用链，
导致其向下覆盖 `.data` 中的 UART 和 ext4 cache 静态状态。

PCI 非对齐的非 volatile 访问和未清零的 DMA ring 是同一路径上的高风险缺陷：前者依赖
编译器和硬件行为，后者会让设备观察到随机 queue 状态。二者不应保留，即使本次 QEMU
复现中的直接崩溃由栈越界触发。

## 修复

- `os/src/arch/loongarch64/qemu/asms/entry.asm`
  - 定义 `BOOT_STACK_SIZE` 和 `MAX_HARTS`，每 hart 启动栈由 4 KiB 提升为 256 KiB。
  - 该内存只用于调度器切换到 per-task kernel stack 前的早期启动阶段；16 hart 总共占用
    4 MiB，在 1 GiB QEMU RAM 配置内可接受。
- `os/src/drivers/virtio/loongarch/pci.rs`
  - 将 ECAM 读取改为对齐 dword 的 `read_volatile()`，再按 byte offset 右移取得目标字段。
  - 写入使用对齐的 `write_volatile()` 并显式断言 dword 对齐。
  - 按 VirtIO PCI capability 布局修正 `cfg_type` (`+3`) 与 BAR (`+4`) 的调试字段。
- `os/src/drivers/virtio/mod.rs`
  - `VirtIoHalCMAImpl::dma_alloc()` 在返回 CMA 页面前按完整页数清零，满足
    `virtio_drivers::Hal` 契约。

## 验证

已执行：

```text
make log TARGET_ARCH=loongarch64
make TARGET_ARCH=loongarch64
timeout 120s make run TARGET_ARCH=loongarch64 \
  > /tmp/loongarch-pci-virtio-net-final.log 2>&1
make TARGET_ARCH=riscv64
timeout 120s make run TARGET_ARCH=riscv64 \
  > /tmp/riscv-virtio-dma-zero-regression.log 2>&1
git diff --check
```

LoongArch64 日志确认 PCI 网卡在 `(0,2,0)` 被发现，网络子系统完成初始化，随后
`task::add_initproc`、用户态 `get_score start!` 和 `shutdown!` 都正常出现；日志中没有
`panic`、`Exception`、`fault`、`Lazy` 或 `poison`。

RISC-V 构建通过，运行日志也完成 initproc 并到达 `shutdown!`，未出现 panic 或 fault。
该运行由 120 秒保护性超时回收 QEMU，因此不将其视为 RISC-V 关机机制验证。

当前 initproc 仅执行启动后关机，故本轮已验证 PCI 枚举、feature 协商、VirtQueue/RX buffer
投递、网络服务注册和用户态切换；未运行产生真实应用层收发流量的 iperf/netperf 回归。
