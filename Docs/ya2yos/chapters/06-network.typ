= 网络模块


== 概述


Ya2yOS 的网络模块位于 `os/src/net/` 与 `os/src/syscall/net/`，整体沿用了 ArceOS 风格的 smoltcp 集成方式，并结合当前内核的文件描述符、任务阻塞和 VirtIO 设备层做了适配。网络功能受 `feature = "net"` 控制；启用后，socket fd 通过 `FileClass::Socket` 纳入统一的文件描述符模型。

当前网络模块的目标是提供 Linux socket API 的主要兼容面，确保网络相关的核心功能能够正常运行。已实现的重点包括 IPv4 TCP/UDP、loopback、VirtIO-net、Unix domain socket、常见 socket syscall、poll/epoll 接入和部分 socket option。

== 架构分层


网络模块可以分为四层：

```text
┌──────────────────────────────────────────────┐
│ syscall/net                                  │
│ socket/bind/listen/accept/connect/send/recv  │
│ getsockopt/setsockopt/getpeername/...        │
├──────────────────────────────────────────────┤
│ net/socket.rs + tcp.rs + udp.rs + unix.rs     │
│ SocketOps、Socket 枚举、TCP/UDP/Unix 实现      │
├──────────────────────────────────────────────┤
│ service.rs + router.rs + wrapper.rs          │
│ smoltcp Interface、SocketSet、路由与调度       │
├──────────────────────────────────────────────┤
│ net/device + drivers/virtio/net.rs           │
│ LoopbackDevice、EthernetDevice、VirtIO-net    │
└──────────────────────────────────────────────┘
```

`Service` 持有 smoltcp `Interface` 和自定义 `Router`；`Router` 同时实现 smoltcp `phy::Device`，将协议栈的 IP 包分发到 loopback 或以太网设备。

== 全局对象


=== SOCKET_SET


```rust
static SOCKET_SET: Lazy<SocketSetWrapper> = Lazy::new(SocketSetWrapper::new);
```

`SOCKET_SET` 封装 smoltcp `SocketSet`，用于保存 TCP/UDP 的底层 smoltcp socket。TCP/UDP 对象只保存 `SocketHandle`，真正的数据缓冲区和协议状态在 `SocketSet` 中。

=== LISTEN_TABLE


```rust
static LISTEN_TABLE: Lazy<ListenTable> = Lazy::new(ListenTable::new);
```

`LISTEN_TABLE` 记录监听中的 TCP 端口。`Router::snoop_tcp_packets()` 会在 IP 包进入 smoltcp 前识别 TCP SYN 包，并通知监听表为 `accept()` 准备已连接 socket handle。

=== SERVICE


```rust
static SERVICE: Once<Mutex<Service>> = Once::new();
```

`SERVICE` 是网络模块的调度核心。`poll_interfaces()` 会反复调用 `Service::poll()`，驱动收包、TCP 状态机、重传定时器和发包队列。

== 初始化流程


`init_network(net_devs)` 在内核启动时初始化网络：

1. 创建 `Router`；
2. 注册 `LoopbackDevice`，配置 `127.0.0.1/8`；
3. 如果探测到 VirtIO-net 设备，则创建 `EthernetDevice("eth0")`，使用 `net/consts.rs` 中的 IP、前缀长度和网关；
4. 添加两类路由规则：`127.0.0.0/8` 走 loopback，默认路由 `0.0.0.0/0` 走 eth0 和网关；
5. 创建 `Service`，将 loopback IP 和 eth0 IP 写入 smoltcp `Interface`；
6. 将 `Service` 保存到全局 `SERVICE`。

RISC-V64 上 VirtIO-net 通过 MMIO 地址探测；LoongArch64 上通过 PCI 枚举得到 `PciTransport`。如果没有物理网卡，网络模块仍可保留 loopback 能力。

== 路由与设备


=== Router


`Router` 内部维护接收/发送包缓冲区、设备列表和路由表：

```rust
pub struct Router {
    rx_buffer: PacketBuffer,
    tx_buffer: PacketBuffer,
    devices: Vec<Box<dyn Device>>,
    table: RouteTable,
}
```

路由表按前缀长度排序，查找时使用最长前缀匹配。`dispatch()` 从 `tx_buffer` 取出 smoltcp 生成的 IP 包，对广播/组播/单播分别处理，并根据路由规则选择下一跳和出口设备。

=== LoopbackDevice


`LoopbackDevice` 在内存中模拟 `lo`：发送时将包写入内部 FIFO，接收时从 FIFO 取出并交给协议栈。它用于本机进程间的 TCP/UDP 回环通信。

=== EthernetDevice


`EthernetDevice` 包装 `NetDeviceImpl`，提供以太网帧收发、ARP 缓存和组播 MAC 映射：

- IPv4 包会被封装为 Ethernet frame 后发送；
- 目标 MAC 未知时发送 ARP 请求，并暂存待发送 IP 包；
- 收到 ARP 请求时会按本机 IP 返回 ARP reply；
- ARP 缓存默认 TTL 为 60 秒；
- IPv4 组播地址会映射到 `01:00:5e:*` 以太网组播 MAC。

== TCP 套接字


TCP 使用 smoltcp TCP socket 作为底层状态机：

```rust
pub struct TcpSocket {
    state: StateLock,
    handle: SocketHandle,
    general: GeneralOptions,
    rx_closed: AtomicBool,
    poll_rx_closed: PollSet,
    memberships: RwLock<Vec<(u32, IpAddress)>>,
}
```

`State` 包括 `Idle`、`Connecting`、`Connected`、`Listening`、`Closed` 和 `Busy`。TCP RX/TX 缓冲区大小由 `net/consts.rs` 中的 `TCP_RX_BUF_LEN`、`TCP_TX_BUF_LEN` 决定。

服务端流程：

1. `socket(AF_INET, SOCK_STREAM, proto)` 创建 `TcpSocket`；
2. `bind()` 设置本地 IP/端口，端口为 0 时分配临时端口；
3. `listen()` 将端口加入 `LISTEN_TABLE`；
4. `accept()` 通过 `LISTEN_TABLE.accept()` 获取已完成连接；
5. `read/recv` 和 `write/send` 映射到 TCP 接收/发送缓冲区。

客户端流程：

1. `connect()` 自动选择源 IP 和临时端口；
2. 调用 smoltcp `connect()` 发起握手；
3. 通过 `poll_interfaces()` 推进握手，连接成功后状态变为 `Connected`；
4. 后续收发与普通流式 socket 一致。

`TcpSocket` 实现了 `File`，因此 `read/write/poll/epoll` 可以直接作用在 TCP fd 上。`poll()` 会根据连接状态返回可读、可写或 RDHUP 事件。

== UDP 套接字


UDP 套接字维护本地端点、可选 peer 和通用选项：

```rust
pub struct UdpSocket {
    handle: SocketHandle,
    local_addr: RwLock<Option<IpEndpoint>>,
    peer_addr: RwLock<Option<(IpEndpoint, IpAddress)>>,
    general: GeneralOptions,
    memberships: RwLock<Vec<(u32, IpAddress)>>,
}
```

`bind()` 会打开 smoltcp UDP socket；端口为 0 时分配临时端口。`connect()` 只记录默认远端和源地址，不进行握手。`sendto/sendmsg` 可以指定目标地址，`send/recv` 使用已连接的 peer。接收路径支持返回源地址和 `MSG_TRUNC` 语义。

UDP 支持 `IP_TTL`、组播加入/离开等部分 IP 层选项。

== Unix Domain Socket


`AF_UNIX` 当前支持 `SOCK_STREAM` 和 `SOCK_DGRAM`，包括 `socketpair()`。Unix socket 不经过 smoltcp，使用内核内存队列传递消息：

```rust
pub enum UnixSocketAddr {
    Unnamed,
    Abstract(Vec<u8>),
    Path(String),
}

pub enum UnixSocketKind {
    Stream,
    Dgram,
}
```

`UNIX_BINDS` 是全局地址表，负责路径/abstract 地址到 socket 的绑定关系。stream socket 的 `listen/connect/accept` 通过 pending 队列建立连接；dgram socket 可按目标地址投递消息。每个 socket 的接收队列上限语义仍较简化，`register()` 尚未接入异步 waker，因此 Unix socket 的 epoll 唤醒能力弱于 TCP/UDP。

== Socket Option


网络模块通过 `Configurable` trait 分发 socket option：

```rust
pub trait Configurable {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> SysResult<bool>;
    fn set_option_inner(&self, opt: SetSocketOption) -> SysResult<bool>;
}
```

当前覆盖的主要选项包括：

- `SOL_SOCKET`：`SO_REUSEADDR`、`SO_SNDBUF`、`SO_RCVBUF`、`SO_KEEPALIVE`、`SO_RCVTIMEO`、`SO_SNDTIMEO`；
- `IPPROTO_IP`：`IP_TTL`、`MCAST_JOIN_GROUP`、`MCAST_LEAVE_GROUP`，`IP_MULTICAST_IF` 目前兼容返回成功；
- `IPPROTO_TCP`：`TCP_NODELAY`；
- `AF_UNIX`：发送/接收缓冲区大小、非阻塞标志、peer credentials、pass credentials 兼容。

未支持的选项返回 `ENOPROTOOPT`。部分 getsockopt 路径仍偏兼容测试导向，并非完整 Linux 语义。

== 系统调用与 File 统一


`sys_socket()` 按 domain/type/protocol 创建 socket：

- `AF_INET + SOCK_STREAM` -> TCP；
- `AF_INET + SOCK_DGRAM` -> UDP；
- `AF_UNIX + SOCK_STREAM/SOCK_DGRAM` -> Unix socket；
- 其他 family/type 返回 `EAFNOSUPPORT`、`ESOCKTNOSUPPORT` 或 `EPROTONOSUPPORT`。

创建出的 socket 以 `FileClass::Socket(Arc<Socket>)` 放入 fd 表：

```rust
pub enum Socket {
    Udp(UdpSocket),
    Tcp(TcpSocket),
    Unix(UnixSocket),
}
```

`bind/listen/accept/connect/shutdown` 通过 `SocketOps` 分发；`sendto/sendmsg/recvfrom/recvmsg` 会解析用户态 `msghdr/iovec/sockaddr`，复制数据并调用对应 socket 的 `send/recv`。

== 数据收发路径


以 TCP 发送为例：

1. 用户态调用 `sendmsg()`；
2. syscall 层读取 `msghdr` 和 iovec，将用户缓冲区整理为 `UserBuffer`；
3. fd 表查到 `FileClass::Socket`，分发到 `TcpSocket::send()`；
4. `TcpSocket::send()` 调用 smoltcp TCP socket 写入发送缓冲区；
5. `poll_interfaces()` 驱动 smoltcp 生成 IP 包，并写入 `Router.tx_buffer`；
6. `Router::dispatch()` 根据路由选择 loopback 或 eth0；
7. `EthernetDevice` 执行 ARP/以太网封装，最终调用 VirtIO-net 驱动发送。

接收路径反向进行：设备收到帧后交给 `EthernetDevice`，IPv4 payload 进入 `Router.rx_buffer`，smoltcp 消费 IP 包并填充对应 socket 的接收缓冲区，用户态通过 `recvmsg()` 取出数据。

== 当前边界


1. *协议范围*：主要覆盖 IPv4 TCP/UDP 和 Unix socket；IPv6 地址解析存在，但完整 IPv6 路由、邻居发现和上层语义并不完整。
2. *驱动方式*：网络通过 `poll_interfaces()` 周期性轮询推进；VirtIO-net 内部已有部分
中断确认和 waker 逻辑，但架构 IRQ 抽象仍有未完成的 enable/disable/acknowledge 钩子，
尚未形成完整的中断驱动收包与 socket 唤醒路径。
3. *socket option*：只覆盖常见选项，部分选项仅兼容返回，`getsockopt` 行为还需继续贴近 Linux。
4. *Unix socket 唤醒*：Unix socket 可读写和 socketpair 已有基础实现，但 waker 注册仍是空实现。
5. *高级能力*：缺少 netlink、raw socket、packet socket、完整防火墙/路由管理、TCP_INFO 等能力。
