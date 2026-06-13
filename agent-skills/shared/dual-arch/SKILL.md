---
name: dual-arch
description: >-
  RISC-V 与 LoongArch64 差异对照。双架构相关内核修改后两侧验证并写文档，见 kernel-change。
---

# 双架构开发对照

> 修改 `arch/` 或页表后：两侧 `make`、记录差异到 problem/，见 [kernel-change](../kernel-change/SKILL.md)。

本项目同时支持 **RISC-V 64**（主开发/评测）和 **LoongArch 64**。

## 快速切换

```bash
make TARGET_ARCH=riscv64
make TARGET_ARCH=loongarch64
```

| 项 | RISC-V | LoongArch |
|----|--------|-----------|
| Rust target | `riscv64gc-unknown-none-elf` | `loongarch64-unknown-none-softfloat` |
| 内核产物 | `kernel-rv` | `kernel-la` |
| 测试镜像 | `sdcard-rv.img` | `sdcard-la.img` |
| QEMU | `qemu-system-riscv64 -machine virt` | `qemu-system-loongarch64` |
| GDB | `gdb-multiarch` | 同左 |

配置：`make_scripts/riscv64.mk` / `loongarch64.mk` 的 `DISK_IMG`、`QEMU_CMD`

## 代码组织

```
os/src/arch/
├── riscv64/       # trap、SV39 页表、memory_layout
└── loongarch64/   # trap、页表、memory_layout、PCI
```

条件编译：

```rust
#[cfg(target_arch = "riscv64")]
#[cfg(target_arch = "loongarch64")]
#[cfg(feature = "riscv64")]      // Cargo feature，Makefile 传入
#[cfg(feature = "loongarch64")]
```

**注意**：`os/Cargo.toml` 的 `default` features 仅用于 rust-analyzer；**编译时由 Makefile 指定**，避免冲突。

## Virtio 驱动差异

| 设备 | RISC-V | LoongArch |
|------|--------|-----------|
| 块设备 | MMIO `virtio-blk-device` | PCI `virtio-blk-pci` |
| 网卡 | MMIO `virtio-net-device` @固定地址 | PCI `virtio-net-pci` |
| 探测 | `drivers/virtio/mod.rs` MMIO | `drivers/virtio/loongarch/pci.rs` |

RISC-V QEMU 片段（`riscv64.mk`）：
```
-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0
-device virtio-net-device,netdev=net,bus=virtio-mmio-bus.1
```

LoongArch 使用 PCI 设备，**不带** `bus=virtio-mmio-bus.0`（会报错）。

## 网络：当前最大差异

`os/src/main.rs`：

```rust
// LoongArch：网络设备容器为空 → 网络栈无网卡
#[cfg(all(feature = "net", target_arch = "loongarch64"))]
let net_devices = DeviceContainer::default();

// RISC-V：探测 virtio-net
#[cfg(all(feature = "net", target_arch = "riscv64"))]
let net_devices = match NetDeviceImpl::try_new_device() { ... };
```

**影响**：所有网络 LTP（accept02、connect01 等）在 LA 上当前必败，除非：
1. 实现 PCI virtio-net 探测并填入 `DeviceContainer`
2. 去掉 `main.rs` 中的 LA 空容器分支

修网络 bug 时先确认 `TARGET_ARCH`。

## 页表 / COW

两架构均有独立实现，COW bug 可能只在一侧复现：

| 架构 | 路径 |
|------|------|
| RISC-V | `arch/riscv64/qemu/page_table.rs` |
| LoongArch | `arch/loongarch64/qemu/page_table.rs` |

修 COW 时**两侧都要验证**（见 [debug-playbook](../debug-playbook/SKILL.md)）。

## 用户态 / 动态链接

| 项 | RISC-V | LoongArch |
|----|--------|-----------|
| initproc 测试路径 | `/musl/...` `/glibc/...` on sdcard | 同结构，不同镜像 |
| ld-linux | `os/src/fs/map_dynamic_link.rs` 按 arch 映射 | 路径不同 |

glibc 程序（iozone 等）失败时检查 interpreter 路径是否匹配当前 arch。

## 上板 vs QEMU

`board` feature（`os/Cargo.toml`）：
- 启用后选真实外存驱动、串口地址等
- 与 `cached_block_dev` 或 `ramdisk` 配合（互斥）
- 竞赛 QEMU 评测**通常不启用**

## 开发检查清单

修跨 arch 功能时：

- [ ] `riscv64` 和 `loongarch64` 均 `make` 通过
- [ ] 页表/trap 改动是否两侧同步
- [ ] virtio 走 MMIO 还是 PCI
- [ ] 网络相关改动是否在 LA 上可测（或注明仅 RV）
- [ ] `user/` 与 `os/` 的 arch feature 一致

## 相关技能

- 编译运行 → [build-and-test](../build-and-test/SKILL.md)
- 网络调试 → [network-debug](../network-debug/SKILL.md)
- COW/futex → [debug-playbook](../debug-playbook/SKILL.md)
