# accept02 组播与 setsockopt 错误传播

## 背景

LTP `accept02` 测试 CVE-2017-8890：listener 调用 `MCAST_JOIN_GROUP` 加入 `224.0.0.0` 后，`accept()` 得到的新 socket **不应**继承组播成员。测试在 accepted fd 上调用 `MCAST_LEAVE_GROUP`，期望 `setsockopt` 失败且 `errno=EADDRNOTAVAIL`。

## 现象

1. **路由断言 panic**：`router.rs` 中 `assert_eq!(rule.src, packet.src_addr())` 失败，`left: 10.0.2.15, right: 127.0.0.1`。listener 加入组播后 `poll_interfaces` 发 IGMP，源地址与路由期望不一致。

2. **virtio token panic**：`index out of bounds: len 128, index 65472 (0xFFC0)`。组播帧错误走 ARP 路径，virtqueue used ring 状态损坏。

3. **TFAIL**：`Multicast group was copied!`。log 显示 accepted fd 上 `MCAST_LEAVE_GROUP` 返回 0，但 socket 层 `memberships` 实际为空。

## 分析

| 问题 | 根因 |
| ------ | ------ |
| IGMP 源地址 | `update_ip_addrs` 须先注册 eth0 再 lo，否则 smoltcp 取第一个 IP 作 IGMP 源 |
| 组播路由 | IPv4 组播包应走非 lo 的 eth0 设备，类似现有 IPv6 分支 |
| 以太网发送 | 组播 IP 须映射 `01:00:5e:xx:xx:xx` 直接发送，不能走 ARP |
| TFAIL 误判 | `sys_setsockopt` 末尾固定 `Ok(0)`，`set_option` 返回的 `EADDRNOTAVAIL` 被丢弃 |
| 组播成员语义 | `accept()` 通过 `new_connected` 创建空 `memberships`，socket 层正确；问题在 syscall 包装 |

曾误判为「组播被复制到 accepted socket」，实为 syscall 未传播错误。

## 修复

1. **`os/src/net/mod.rs`**：eth0 IP 先于 lo 注册
2. **`os/src/net/router.rs`**：IPv4 组播 dispatch 到非 lo 设备；注释掉无意义的 `rule.src` 断言
3. **`os/src/net/device/ethernet.rs`**：`ipv4_multicast_mac()`；`send()` 组播直发；接收侧接受组播 MAC 帧
4. **`os/src/net/tcp.rs` / `udp.rs`**：实现 `JoinGroup` / `LeaveGroup`（per-socket `memberships` + iface 组播表）
5. **`os/src/syscall/net/opt.rs`**：注册 `MCAST_JOIN_GROUP` / `MCAST_LEAVE_GROUP`；`match { ... }?` 传播错误
6. **`os/src/drivers/virtio/net.rs`**：`token_index()` 边界检查，非法 token 返回 `BadState` 而非 panic

## 涉及文件

- `os/src/net/mod.rs`
- `os/src/net/router.rs`
- `os/src/net/device/ethernet.rs`
- `os/src/net/tcp.rs`
- `os/src/net/udp.rs`
- `os/src/syscall/net/opt.rs`
- `os/src/drivers/virtio/net.rs`

## 验证

```text
TPASS: Multicast group was not copied: EADDRNOTAVAIL (99)
Summary: passed 1, failed 0
```

末尾 `ext4_fopen: /tmp/LTP_*/ltp_accept02_2, rc=2`（ENOENT）为 LTP cleanup 时序问题，不影响 TPASS。

## 遗留

`invalid virtqueue token 65472` 在边界检查后降为 WARN，TX 回收仍偶发 `BadState`，根因待进一步排查。
