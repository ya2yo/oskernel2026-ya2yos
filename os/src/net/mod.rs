//! 本模块来源于ArceOS 网络模块。
//!
//!
//! # 初始化流程
//! 1. 系统启动时探测到 VirtIO 网络设备。
//! 2. 调用 [`init_network`] 传入探测到的设备容器。
//! 3. 配置环回接口 (lo) 和以太网接口 (eth0)。
//! 4. 设置默认路由规则。
//!
//! [smoltcp]: https://github.com/smoltcp-rs/smoltcp

mod consts;
mod device;
mod general;
mod listen_table;
/// Socket option types and the [`Configurable`](options::Configurable) trait.
pub mod options;
mod router;
mod service;
mod socket;
pub(crate) mod state;
/// TCP socket implementation.
pub mod tcp;
/// UDP socket implementation.
pub mod udp;
mod unix;
/// Unix domain socket implementation.
// pub mod unix;
/// Vsock socket implementation.
// #[cfg(feature = "vsock")]
// pub mod vsock;
mod wrapper;

pub use self::device::{EthernetDevice, LoopbackDevice};
pub use self::socket::*;
use self::{
    consts::{GATEWAY, IP, IP_PREFIX},
    listen_table::ListenTable,
    router::{Router, Rule},
    service::Service,
    wrapper::SocketSetWrapper,
};
use crate::drivers::{BaseDriver, DeviceContainer, NetDeviceImpl, NetDriverOps};
use alloc::{borrow::ToOwned, boxed::Box};
use log::{info, warn};
use smoltcp::wire::{EthernetAddress, Ipv4Address, Ipv4Cidr};
use spin::Mutex;
use spin::{Lazy, Once};
pub use unix::*;
use virtio_drivers::device::net::VirtIONet;

/// 全局监听表，用于跟踪所有处于监听状态的套接字。
static LISTEN_TABLE: Lazy<ListenTable> = Lazy::new(ListenTable::new);
/// 全局套接字集合，管理所有活跃的网络连接。
static SOCKET_SET: Lazy<SocketSetWrapper> = Lazy::new(SocketSetWrapper::new);
/// 网络服务核心单例，负责接口调度和协议栈处理。
static SERVICE: Once<Mutex<Service>> = Once::new();
/// 获取网络服务实例的互斥锁。
/// 如果在调用 `init_network` 之前调用此函数，将会触发 panic。
fn get_service() -> spin::MutexGuard<'static, Service> {
    SERVICE
        .get()
        .expect("Network service not initialized")
        .lock()
}

/// 初始化网络子系统。
///
/// 该函数执行以下操作：
/// 1. 初始化路由管理器。
/// 2. 注册并配置环回接口 (Loopback, 127.0.0.1)。
/// 3. 从 `net_devs` 容器中提取一个物理/虚拟网卡（如果存在），配置为 `eth0`。
/// 4. 根据 `consts` 中的配置设置 IP 地址、子网掩码及默认网关。
/// 5. 启动全局网络服务单例。
///
/// # 参数
/// - `net_devs`: 包含探测到的网络设备驱动实例的容器。
pub fn init_network(mut net_devs: DeviceContainer<NetDeviceImpl>) {
    println!("Initialize network subsystem...");

    let mut router = Router::new();
    let lo_dev = router.add_device(Box::new(LoopbackDevice::new()));
    // 配置环回loopback接口
    let lo_ip = Ipv4Cidr::new(Ipv4Address::new(127, 0, 0, 1), 8);
    router.add_rule(Rule::new(
        lo_ip.into(),
        None,
        lo_dev,
        lo_ip.address().into(),
    ));
    // 配置以太网物理接口
    let eth0_ip = if let Some(dev) = net_devs.take_one() {
        info!("  use NIC 0: {:?}", dev.device_name());

        let eth0_address = EthernetAddress(dev.mac_address().0);
        let eth0_ip = Ipv4Cidr::new(IP.parse().expect("Invalid IPv4 address"), IP_PREFIX);

        let eth0_dev = router.add_device(Box::new(EthernetDevice::new(
            "eth0".to_owned(),
            dev,
            eth0_ip,
        )));
        // 添加默认路由规则
        router.add_rule(Rule::new(
            Ipv4Cidr::new(Ipv4Address::UNSPECIFIED, 0).into(),
            Some(GATEWAY.parse().expect("Invalid gateway address")),
            eth0_dev,
            eth0_ip.address().into(),
        ));

        info!("eth0:");
        info!("  mac:  {}", eth0_address);
        info!("  ip:   {}", eth0_ip);

        Some(eth0_ip)
    } else {
        warn!("  No network device found!");
        None
    };
    // 打印当前所有路由接口
    for dev in &router.devices {
        info!("Device: {}", dev.name());
    }
    // 构造并启动服务
    let mut service = Service::new(router);
    service.iface.update_ip_addrs(|ip_addrs| {
        ip_addrs.push(lo_ip.into()).unwrap();
        if let Some(eth0_ip) = eth0_ip {
            ip_addrs.push(eth0_ip.into()).unwrap();
        }
    });
    SERVICE.call_once(|| Mutex::new(service));
}

// /// Init vsock subsystem by vsock devices.
// #[cfg(feature = "vsock")]
// pub fn init_vsock(mut vsock_devs: DeviceContainer<VirtIoNetDevImpl>) {
//     use self::device::register_vsock_device;
//     info!("Initialize vsock subsystem...");
//     if let Some(dev) = vsock_devs.take_one() {
//         info!("  use vsock 0: {:?}", dev.device_name());
//         if let Err(e) = register_vsock_device(dev) {
//             warn!("Failed to initialize vsock device: {:?}", e);
//         }
//     } else {
//         warn!("  No vsock device found!");
//     }
// }

/// 轮询网络接口以处理待办事件。
///
/// 该函数必须在系统的外层循环或中断处理中被定期调用。
/// 它负责触发 smoltcp 的协议栈处理，包括：
/// - 从硬件接收缓冲区读取数据包。
/// - 处理重传定时器。
/// - 将待发送的数据包写入硬件。
pub fn poll_interfaces() {
    while get_service().poll(&mut SOCKET_SET.inner.lock()) {}
}
