use alloc::{boxed::Box, vec, vec::Vec};

use log::{debug, warn};
use smoltcp::{
    iface::SocketSet,
    phy::{DeviceCapabilities, Medium},
    storage::PacketMetadata,
    time::Instant,
    wire::{IpAddress, IpCidr, IpProtocol, IpVersion, Ipv4Packet, Ipv6Packet, TcpPacket},
};

use super::{
    consts::{SOCKET_BUFFER_SIZE, STANDARD_MTU},
    device::Device,
    LISTEN_TABLE,
};

/// 路由规则
#[derive(Debug)]
pub struct Rule {
    pub filter: IpCidr,         // 目标地址匹配网段
    pub via: Option<IpAddress>, // 下一跳网关地址
    pub dev: usize,             // 该网段对应的设备索引
    pub src: IpAddress,         // 本地源地址
}

impl Rule {
    pub fn new(filter: IpCidr, via: Option<IpAddress>, dev: usize, src: IpAddress) -> Self {
        Self {
            filter,
            via,
            dev,
            src,
        }
    }
}
/// 简单的数据包缓冲区类型，底层使用 smoltcp 的存储结构
type PacketBuffer = smoltcp::storage::PacketBuffer<'static, ()>;

/// 路由表
#[derive(Debug)]
pub struct RouteTable {
    rules: Vec<Rule>,
}
impl RouteTable {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }
    /// 添加路由规则，并按掩码长度降序排序
    /// 这样在查找时可以实现“最长前缀匹配”逻辑
    pub fn add_rule(&mut self, rule: Rule) {
        let idx = self
            .rules
            .binary_search_by(|it| rule.filter.prefix_len().cmp(&it.filter.prefix_len()))
            .unwrap_or_else(|idx| idx);
        self.rules.insert(idx, rule);
    }
    /// 根据目标 IP 地址查找匹配的路由规则
    pub fn lookup(&self, dst: &IpAddress) -> Option<&Rule> {
        self.rules
            .iter()
            .find(|rule| rule.filter.contains_addr(dst))
    }
}
/// 路由器核心结构
/// 它本身实现了 smoltcp::phy::Device 特性，因此对协议栈来说它像是一个“网卡”
/// 但实际上它内部管理着多个真实的物理设备
pub struct Router {
    rx_buffer: PacketBuffer, // 接收缓冲区，存放从各个物理设备收到的包
    tx_buffer: PacketBuffer, // 发送缓冲区，存放协议栈准备发出的包
    pub(crate) devices: Vec<Box<dyn Device>>, // 路由器连接的所有网卡设备
    pub(crate) table: RouteTable, // 路由表
}
impl Router {
    pub fn new() -> Self {
        // 初始化 RX/TX 环形缓冲区，大小由常量定义
        let rx_buffer = PacketBuffer::new(
            vec![PacketMetadata::EMPTY; SOCKET_BUFFER_SIZE],
            vec![0u8; STANDARD_MTU * SOCKET_BUFFER_SIZE],
        );
        let tx_buffer = PacketBuffer::new(
            vec![PacketMetadata::EMPTY; SOCKET_BUFFER_SIZE],
            vec![0u8; STANDARD_MTU * SOCKET_BUFFER_SIZE],
        );
        Self {
            rx_buffer,
            tx_buffer,
            devices: Vec::new(),
            table: RouteTable::new(),
        }
    }

    pub fn add_rule(&mut self, rule: Rule) {
        self.table.add_rule(rule);
        debug!("[add_rule] {:?}", self.table);
    }

    pub fn add_device(&mut self, device: Box<dyn Device>) -> usize {
        self.devices.push(device);
        self.devices.len() - 1 // 返回新设备的索引
    }
    /// 轮询读取物理设备上的原始数据包进入路由器的 rx_buffer
    pub fn poll(&mut self, timestamp: Instant) {
        for dev in &mut self.devices {
            while !self.rx_buffer.is_full() && dev.recv(&mut self.rx_buffer, timestamp) {}
        }
    }

    /// Inspect the next ingress packet before smoltcp consumes it.
    ///
    /// A passive TCP socket must exist before its SYN reaches smoltcp. Peeking
    /// avoids the former dequeue, heap-copy, and re-enqueue cycle for every
    /// packet currently buffered by the router.
    pub fn snoop_next_tcp_packet(&mut self, sockets: &mut SocketSet<'_>) {
        if let Ok(((), packet)) = self.rx_buffer.peek() {
            snoop_tcp_packet(packet, sockets);
        }
    }

    /// 分发将 tx_buffer 中的包根据路由表发送到具体的物理设备上
    pub fn dispatch(&mut self, timestamp: Instant) -> bool {
        let mut poll_next = false;
        // 循环从发送队列中取出数据包
        while let Ok(((), packet)) = self.tx_buffer.dequeue() {
            match IpVersion::of_packet(packet).expect("got invalid IP packet") {
                IpVersion::Ipv4 => {
                    let packet = smoltcp::wire::Ipv4Packet::new_checked(packet)
                        .expect("got invalid IPv4 packet");
                    let dst_addr = IpAddress::Ipv4(packet.dst_addr());
                    // 处理广播包,发送给所有设备
                    if packet.dst_addr().is_broadcast() {
                        let buf = packet.into_inner();
                        for dev in &mut self.devices {
                            poll_next |= dev.send(dst_addr, buf, timestamp);
                        }
                    } else if packet.dst_addr().is_multicast() {
                        let buf = packet.into_inner();
                        for dev in &mut self.devices {
                            if dev.name() != "lo" {
                                poll_next |= dev.send(dst_addr, buf, timestamp);
                            }
                        }
                    } else {
                        // 单播包,查表路由
                        let Some(rule) = self.table.lookup(&dst_addr) else {
                            warn!("No route found for destination: {}", dst_addr);
                            continue;
                        };
                        // 路由表上的对应包的源地址和 packet 的源地址不一定相同
                        // debug!("packet dst ip={}", IpAddress::Ipv4(packet.dst_addr()));
                        // assert_eq!(rule.src, IpAddress::Ipv4(packet.src_addr()));

                        let next_hop = rule.via.unwrap_or(dst_addr);
                        let dev = &mut self.devices[rule.dev];
                        poll_next |= dev.send(next_hop, packet.into_inner(), timestamp);
                    }
                }
                IpVersion::Ipv6 => {
                    let packet = smoltcp::wire::Ipv6Packet::new_checked(packet)
                        .expect("got invalid IPv6 packet");
                    let dst_addr = IpAddress::Ipv6(packet.dst_addr());
                    if packet.dst_addr().is_multicast() {
                        let buf = packet.into_inner();
                        for dev in &mut self.devices {
                            poll_next |= dev.send(dst_addr, buf, timestamp);
                        }
                    } else {
                        let Some(rule) = self.table.lookup(&dst_addr) else {
                            warn!("No route found for destination: {}", dst_addr);
                            continue;
                        };
                        assert_eq!(rule.src, IpAddress::Ipv6(packet.src_addr()));

                        let next_hop = rule.via.unwrap_or(dst_addr);
                        let dev = &mut self.devices[rule.dev];
                        poll_next |= dev.send(next_hop, packet.into_inner(), timestamp);
                    }
                }
            }
        }
        poll_next
    }
}
/// 发送令牌，smoltcp 发送数据包时的抽象回调
pub struct TxToken<'a>(&'a mut PacketBuffer);

impl smoltcp::phy::TxToken for TxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // 从 tx_buffer 申请空间并执行闭包
        f(self
            .0
            .enqueue(len, ())
            .expect("This was checked before creating the TxToken"))
    }
}
/// TCP 包“窃听”函数
/// 用于在包进入协议栈之前检测是否有针对监听端口的 TCP SYN 请求
fn snoop_tcp_packet(buf: &[u8], sockets: &mut SocketSet<'_>) {
    let (protocol, src_addr, dst_addr, payload) = match IpVersion::of_packet(buf).unwrap() {
        IpVersion::Ipv4 => {
            let packet = Ipv4Packet::new_unchecked(buf);
            // IPv4 reassembly happens inside smoltcp. Neither a non-initial
            // fragment nor a first fragment contains a complete TCP packet
            // suitable for passive-open inspection.
            if packet.more_frags() || packet.frag_offset() != 0 {
                return;
            }
            (
                packet.next_header(),
                IpAddress::Ipv4(packet.src_addr()),
                IpAddress::Ipv4(packet.dst_addr()),
                packet.payload(),
            )
        }
        IpVersion::Ipv6 => {
            let packet = Ipv6Packet::new_unchecked(buf);
            (
                packet.next_header(),
                IpAddress::Ipv6(packet.src_addr()),
                IpAddress::Ipv6(packet.dst_addr()),
                packet.payload(),
            )
        }
    };
    // 如果是 TCP 协议且是第一次握手
    if protocol == IpProtocol::Tcp {
        let Ok(tcp_packet) = TcpPacket::new_checked(payload) else {
            return;
        };
        let src_addr = (src_addr, tcp_packet.src_port()).into();
        let dst_addr = (dst_addr, tcp_packet.dst_port()).into();
        let is_first = tcp_packet.syn() && !tcp_packet.ack();
        if is_first {
            // 通知监听表，处理被动打开逻辑
            LISTEN_TABLE.incoming_tcp_packet(src_addr, dst_addr, sockets);
        }
    }
}
/// 接收令牌，smoltcp 接收数据包时的抽象
pub struct RxToken<'a>(&'a [u8]);

impl<'a> smoltcp::phy::RxToken for RxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(self.0) // 直接传递包数据给协议栈处理
    }
    // /// 在协议栈正式处理包之前的预处理阶段
    // fn preprocess(&self, sockets: &mut SocketSet) {
    //     snoop_tcp_packet(self.0, sockets);
    // }
}
/// 为 Router 实现 Device 特性，使其能作为 smoltcp 的后端
impl smoltcp::phy::Device for Router {
    type RxToken<'a> = RxToken<'a>;
    type TxToken<'a> = TxToken<'a>;
    /// 协议栈尝试接收一个包
    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.rx_buffer.is_empty() || self.tx_buffer.is_full() {
            None
        } else {
            // 返回 RX 令牌和 TX 令牌
            Some((
                RxToken(self.rx_buffer.dequeue().unwrap().1),
                TxToken(&mut self.tx_buffer),
            ))
        }
    }
    /// 协议栈尝试发送一个包
    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        if self.tx_buffer.is_full() {
            None
        } else {
            Some(TxToken(&mut self.tx_buffer))
        }
    }
    /// 报告设备能力
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = STANDARD_MTU;
        caps.max_burst_size = Some(SOCKET_BUFFER_SIZE);
        caps
    }
}
