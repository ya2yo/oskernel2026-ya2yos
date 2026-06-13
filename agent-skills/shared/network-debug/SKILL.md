---
name: network-debug
description: >-
  调试 smoltcp、virtio-net、setsockopt 与组播。网络相关内核修改时配合 kernel-change
  与 doc-writing 使用。
---

# 网络协议栈调试

> 流程与文档：[kernel-change](../kernel-change/SKILL.md)、[doc-writing](../doc-writing/SKILL.md)。

基础知识见 [network-stack](../network-stack/SKILL.md)。本文聚焦**调试路径与常见陷阱**。

## 数据流

```
用户态 syscall
  → os/src/syscall/net/     (socket, opt, io, addr)
  → os/src/net/socket.rs    (Socket 分发)
  → tcp.rs / udp.rs         (smoltcp socket + 状态机)
  → poll_interfaces()       (驱动收发包)
  → device/ethernet.rs      (ARP / 组播 MAC)
  → drivers/virtio/net.rs   (virtqueue TX/RX)
```

**关键**：`poll_interfaces()` 必须在调度循环中被调用，否则 TCP 握手/IGMP 无法推进。

## 初始化顺序

`os/src/net/mod.rs` → `init_network` / `update_ip_addrs`

- eth0 (`10.0.2.15`) 应**先于** lo (`127.0.0.1`) 注册，否则 IGMP 源地址错误
- LoongArch：`main.rs` 可能传入空 `DeviceContainer`，网络被禁用——先确认 `#[cfg(target_arch)]` 分支

## setsockopt 陷阱

文件：`os/src/syscall/net/opt.rs`

```rust
match level { ... }?;  // 必须 ? 传播 set_option 错误
Ok(0)
```

若省略 `?`，`LeaveGroup` 返回 `EADDRNOTAVAIL` 仍会向用户态报告成功 → accept02 TFAIL。

组播选项路径：
- `MCAST_JOIN_GROUP` / `MCAST_LEAVE_GROUP` → `group_req`
- socket 级 `memberships: Vec<(if_index, IpAddress)>` 在 `tcp.rs`/`udp.rs`
- smoltcp `iface.join_multicast_group` 是**接口级**；per-socket 状态靠 `memberships` 维护
- `accept()` 新 socket 的 `memberships` 必须为空（`new_connected`）

## 组播 / IGMP

| 问题 | 位置 | 修复方向 |
|------|------|----------|
| 组播走 ARP | `device/ethernet.rs` `send()` | 组播地址用 `01:00:5e:xx:xx:xx`，不走 ARP |
| 路由断言失败 | `router.rs` dispatch | 组播走非 lo 设备；源地址与 iface 一致 |
| IGMP 源地址错误 | `net/mod.rs` ip 注册顺序 | eth0 先于 lo |

## virtio-net 调试

文件：`os/src/drivers/virtio/net.rs`

常见日志：
```
invalid virtqueue token 65472 (queue size 128)
recycle_tx_buffers failed: BadState
```

- token `65472 (0xFFC0)` 超出队列大小 128 → used ring 损坏或回收逻辑错误
- 已有边界检查防 panic，但需查 **为何 poll_transmit 返回非法 token**
- 多发于 IGMP/组播 TX 后；检查 TX descriptor 提交与 used ring 索引

调试步骤：
1. `make log` 复现
2. 在 `send` / `poll_transmit` / `recycle_tx_buffers` / `token_index` 加临时日志
3. 确认组播帧不再走 ARP 路径

## accept / connect 阻塞

- `tcp.rs` `accept()` → `recv_poller` + `poll_interfaces` + `LISTEN_TABLE`
- `block_on` 中避免多余 `Arc` 强引用（见 problem.md Pending 死循环）
- `connect` 返回 `EINPROGRESS` 用于非阻塞；阻塞模式应等完成

## 日志关键词

```bash
rg -a -n "TCP connection|joined multicast|left multicast|invalid virtqueue|setsockopt|sys_accept" log.ans
```

## 相关文件速查

| 任务 | 文件 |
|------|------|
| 新 socket 选项 | `syscall/net/opt.rs` + `net/options.rs` `Configurable` |
| TCP 状态 | `net/tcp.rs`, `net/state.rs` |
| 监听表 | `net/listen_table.rs` |
| 路由 | `net/router.rs` |
| 网卡驱动 | `drivers/virtio/net.rs` |
| 以太网层 | `net/device/ethernet.rs` |

## LoongArch 注意

- PCI virtio 路径：`drivers/virtio/loongarch/pci.rs`
- 若 `main.rs` 跳过 net 初始化，所有网络测试在 LA 上必败
- 修网络前先确认当前 arch 是否启用了 virtio-net
