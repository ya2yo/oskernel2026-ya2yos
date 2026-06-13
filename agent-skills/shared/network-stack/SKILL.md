---
name: network-stack
description: >-
  smoltcp 网络栈模块地图。实现网络功能时先读 kernel-change 与 network-debug，改后写文档。
---

# 网络协议栈知识

> [kernel-change](../kernel-change/SKILL.md) · 调试 [network-debug](../network-debug/SKILL.md) · 文档 [doc-writing](../doc-writing/SKILL.md)

基于 StarryOS + smoltcp。

## 模块地图

| 模块 | 路径 | 职责 |
|------|------|------|
| 初始化 | `os/src/net/mod.rs` | `init_network`、IP 地址、poll |
| Socket 分发 | `os/src/net/socket.rs` | `Socket` enum、`SocketOps` trait |
| TCP | `os/src/net/tcp.rs` | 状态机、accept/connect、memberships |
| UDP | `os/src/net/udp.rs` | 无连接收发 |
| Unix | `os/src/net/unix.rs` | 域套接字 |
| 监听表 | `os/src/net/listen_table.rs` | 端口 → 待 accept 连接队列 |
| 路由 | `os/src/net/router.rs` | 包分发、组播路由 |
| 以太网 | `os/src/net/device/ethernet.rs` | ARP、组播 MAC |
| 选项 | `os/src/net/options.rs` | `Configurable` trait |
| 全局 socket 集 | `os/src/net/socket_set.rs` | smoltcp `SocketSet` |
| Syscall | `os/src/syscall/net/` | socket/bind/connect/opt/io |
| 驱动 | `os/src/drivers/virtio/net.rs` | virtqueue TX/RX |

## Socket 抽象

```rust
// os/src/net/socket.rs
pub struct Socket(SocketInner);

pub enum SocketInner {
    Tcp(TcpSocket),
    Udp(UdpSocket),
    Unix(UnixSocket),
}
```

`SocketOps` trait：`bind`、`listen`、`accept`、`connect`、`send`、`recv`、`shutdown` 等。

## TCP 状态

`os/src/net/state.rs` + `tcp.rs`：

```
Idle → (bind) → Idle
Idle → (listen) → Listening
Listening → (accept) → Connected  // 新 TcpSocket via new_connected
Idle/Bound → (connect) → Connecting → Connected
```

- `accept()` 创建**新** `TcpSocket`，`memberships` 为空（CVE-2017-8890）
- 阻塞 I/O 通过 `general.recv_poller` + `block_on` + `poll_interfaces()`

## listen_table

监听端口上的已完成握手连接以 `SocketHandle` 入队，`accept` 时取出并 `new_connected`。

## 组播

两层状态：
1. **per-socket** `memberships: Vec<(if_index, IpAddress)>` — 决定 setsockopt 语义
2. **smoltcp iface** `join_multicast_group` — 接口级，发 IGMP

`JoinGroup` / `LeaveGroup` 在 `tcp.rs`、`udp.rs` 的 `set_option_inner`。

支持的 setsockopt（`syscall/net/opt.rs`）：
- `MCAST_JOIN_GROUP` (42)、`MCAST_LEAVE_GROUP` (45)
- `SO_REUSEADDR`、`TCP_NODELAY`、`IP_TTL` 等

## poll 循环

```rust
// 必须在调度中调用
net::poll_interfaces();
```

驱动收包 → smoltcp 处理 → 可能唤醒阻塞的 socket 操作。

## 系统调用映射

| syscall | 实现 |
|---------|------|
| socket | `syscall/net/socket.rs` |
| bind/listen/accept/connect | 同上 |
| send/recv/sendto/recvfrom | `syscall/net/io.rs` |
| setsockopt/getsockopt | `syscall/net/opt.rs` |
| getsockname/getpeername | `syscall/net/addr.rs` |

## IP 地址

- eth0: `10.0.2.15`（QEMU user netdev）
- lo: `127.0.0.1`
- **注册顺序**：eth0 先于 lo（IGMP 源地址）

## 调试

```bash
make log
rg -a -n "TCP connection|multicast|virtqueue|setsockopt" log.ans
```

## 参考

- [smoltcp 文档](https://docs.rs/smoltcp/)
- [network-debug](../network-debug/SKILL.md)
- [dual-arch](../dual-arch/SKILL.md) — LoongArch 网络当前未启用
