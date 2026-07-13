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

## 应用层回归用例

启动成功只证明 PCI 枚举、feature 协商、VirtQueue 建立和 RX buffer 投递没有立即破坏
内存，不能证明网卡能够完成真实收发。因此新增 `netdev_test`，并把同一组 cases 接入当前
initproc 入口：

- `udp_loopback_baseline`
  - 在 `127.0.0.1` 上完成 UDP payload 往返，只验证 socket syscall、UDP 和 loopback
    设备，是后续用例的协议栈对照组；该项通过不能单独证明网卡正常。
- `dns_round_trip_once`
  - 向 QEMU SLIRP 虚拟 DNS `10.0.2.3:53` 查询 `localhost`，校验响应 transaction ID、
    QR bit、源 IP 和源端口。该地址不属于 loopback，路由会选择 `eth0`，因此请求和响应
    都会经过 LoongArch VirtIO PCI 网卡。
- `dns_descriptor_recycling_stress`
  - 在同一 socket 上连续完成 160 次 DNS 请求/响应。内核 VirtIO 队列大小为 128，成功
    越过该边界可发现 TX buffer、TX descriptor 或 RX descriptor 未回收导致的耗尽问题。
- `nonblocking_timeout_path`
  - 向 SLIRP gateway 的关闭 UDP 端口发送探测包，并确认 250 ms 内以用户态截止时间结束，
    验证无响应路径不会永久阻塞或让驱动进入错误状态。

所有接收 socket 都使用 `SOCK_NONBLOCK`，只对 `EAGAIN/EINTR` 重试，并由 `get_time()`
限制截止时间。测试以 `TPASS/TFAIL/Summary` 输出明确结果，任何失败都会使
`netdev_test` 返回非零状态。为支持无 libc 的 Rust 用户程序，用户库同时补充了 Linux
`sockaddr_in` 布局和 `bind/sendto/recvfrom` 的薄 syscall 包装。

## 验证

已执行：

```text
make log TARGET_ARCH=loongarch64
make TARGET_ARCH=loongarch64
timeout 120s make run TARGET_ARCH=loongarch64 \
  > /tmp/loongarch-netdev-tests-final.log 2>&1
make TARGET_ARCH=riscv64
timeout 120s make run TARGET_ARCH=riscv64 \
  > /tmp/riscv-netdev-tests.log 2>&1
rustfmt --edition 2018 --check \
  user/src/net.rs user/src/syscall/socket.rs \
  user/src/bin/netdev_test.rs user/src/bin/netdev_test/cases.rs
git diff --check
```

LoongArch64 日志确认 PCI 网卡在 `(0,2,0)` 被发现，网络子系统完成初始化，随后
`task::add_initproc` 和用户态测试正常执行。应用层结果为：

```text
TPASS: udp_loopback_baseline
TPASS: dns_round_trip_once
TINFO: completed 160 / 160 DNS round trips
TPASS: dns_descriptor_recycling_stress
TPASS: nonblocking_timeout_path
Summary: netdev passed 4 failed 0
```

RISC-V 构建及同组 QEMU 运行也为 `passed 4 failed 0`。两架构日志均到达 `shutdown!`，
没有 `panic`、`Exception`、`fault`、`BadState` 或 `invalid virtqueue token`。因此当前验证已从
“仅完成网卡初始化”提升为真实应用层 UDP TX/RX，并覆盖超过单轮 VirtQueue 容量的持续收发；
尚未覆盖 TCP、大包分片、并发 socket 或 iperf/netperf 吞吐场景。
