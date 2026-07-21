# RISC-V VirtIO-MMIO 网卡自动总线分配

## 背景

RISC-V 使用 QEMU `virt` 平台和 VirtIO-MMIO 设备。评测机启动参数显式将块设备接入
`virtio-mmio-bus.0`，但网卡未指定 `bus`。

## 现象

`log.ans` 在网络初始化阶段输出：

```text
No usable VirtIO Net MMIO device at 0xffffffc010002000: ZeroDeviceId
No network device found!
```

## 分析

内核原先将网卡固定为物理地址 `0x10002000`，即 `virtio-mmio-bus.1`。使用当前 QEMU 的
monitor `info qtree` 检查同一组启动参数，确认块设备在 `.0` / `0x10001000`，而未显式指定
总线的网卡被自动放入 `.7` / `0x10008000`。此外，内核页表只映射了旧网卡页，即使仅改驱动
基址也无法安全访问新地址。

## 根因

评测机配置移除了网卡的显式总线绑定后，QEMU 自动设备分配位置与内核写死的 `.1` 地址不一致。

## 修复

- 将 RISC-V 网卡驱动的 `VIRTIO_NET_BASE` 改为 `0x10008000`。
- 将 RISC-V 内核页表的 VirtIO 网卡 MMIO 映射同步改为 `0x10008000`。
- 保持 `make_scripts/riscv64.mk` 的评测机 QEMU 参数不变。

这个最小修复适用于当前 QEMU `virt` 自动分配规则。若评测环境未来调整 VirtIO 设备数或顺序，
应解析 boot DTB 的 `virtio,mmio` 节点来发现网卡，而不是继续依赖固定槽位。

## 涉及文件

- `os/src/drivers/mod.rs`
- `os/src/arch/riscv64/qemu/memory_layout.rs`

## 验证

- `make TARGET_ARCH=riscv64 build-arch`：通过。
- 使用评测机 QEMU 参数启动 RISC-V：日志出现 `Initialize network subsystem...`，不再出现
  `No usable VirtIO Net MMIO device` 或 `No network device found`；后续用户态网络服务收到 HTTP
  请求。
- 运行由 120 秒 timeout 截止，未验证完整评测集。
