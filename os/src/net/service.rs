use crate::task::sleep_until;
use crate::timer::{wall_time_nanos, Timespec, NANOS_PER_MICROS};
use alloc::boxed::Box;
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Waker},
};
use smoltcp::{
    iface::{Interface, SocketSet},
    time::Instant,
    wire::{HardwareAddress, IpAddress, IpListenEndpoint},
};

use super::{router::Router, SOCKET_SET};
/// 获取当前系统的 Instant 时间（smoltcp 专用格式）
/// 将系统墙上时间（纳秒）转换为 smoltcp 的微秒单位
fn now() -> Instant {
    Instant::from_micros_const((wall_time_nanos() / NANOS_PER_MICROS) as i64)
}
/// 网络服务核心结构体
pub struct Service {
    /// smoltcp 网络接口，负责处理协议栈逻辑
    pub iface: Interface,
    /// 自定义路由器，管理多个物理/虚拟设备和路由表
    router: Router,
    /// 存储当前等待超时的 Future，用于异步唤醒协议栈（如 TCP 重传计时）
    timeout: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
}
impl Service {
    /// 创建一个新的网络服务实例
    pub fn new(mut router: Router) -> Self {
        // 创建 smoltcp 配置，使用 IP 层硬件地址
        let config = smoltcp::iface::Config::new(HardwareAddress::Ip);
        // 初始化接口，router 在此处充当 Device
        let iface = Interface::new(config, &mut router, now());

        Self {
            iface,
            router,
            timeout: None,
        }
    }
    /// 轮询网络堆栈，处理数据包的收发和状态更新
    /// 返回值：如果有数据包被成功发送或处理，通常返回 true（取决于 router.dispatch）
    pub fn poll(&mut self, sockets: &mut SocketSet) -> bool {
        let timestamp = now();

        self.router.poll(timestamp);
        self.router.snoop_tcp_packets(sockets);
        self.iface.poll(timestamp, &mut self.router, sockets);
        self.router.dispatch(timestamp)
    }
    /// 根据目的 IP 地址查找路由表，获取对应的源 IP 地址
    pub fn get_source_address(&self, dst_addr: &IpAddress) -> IpAddress {
        let Some(rule) = self.router.table.lookup(dst_addr) else {
            panic!("no route to destination: {dst_addr}");
        };
        rule.src
    }
    /// 根据监听端点返回适用的设备掩码（Bitmask）
    /// 用于多网卡环境下，确定某个监听 Socket 应该关联哪些物理设备
    pub fn device_mask_for(&self, endpoint: &IpListenEndpoint) -> u32 {
        match endpoint.addr {
            Some(addr) => self
                .router
                .table
                .lookup(&addr)
                .map_or(0, |it| 1u32 << it.dev),
            None => u32::MAX,
        }
    }
    /// 注册异步唤醒器（Waker）
    /// 该函数确保当网络协议栈需要处理（如超时）或底层设备有新包时，能够唤醒当前的异步任务
    pub fn register_waker(&mut self, mask: u32, waker: &Waker) {
        // 询问协议栈下一次定时任务是什么时候？
        let next = self.iface.poll_at(now(), &SOCKET_SET.inner.lock());

        if let Some(t) = next {
            let next = Timespec::from_micros(t.total_micros() as _);

            // 清理旧的超时 Future
            self.timeout = None;
            // 创建一个新的睡眠 Future，到时间后唤醒 waker
            let mut fut = Box::pin(sleep_until(next));
            let mut cx = Context::from_waker(waker);
            // 尝试轮询一次睡眠 Future
            if fut.as_mut().poll(&mut cx).is_ready() {
                waker.wake_by_ref();
                return;
            } else {
                self.timeout = Some(fut);
            }
        }
        // 为掩码指定的每个物理设备注册 Waker
        // 当底层网卡收到数据包触发中断时，会通过这个 Waker 唤醒用户态的服务进程
        for (i, device) in self.router.devices.iter().enumerate() {
            if mask & (1 << i) != 0 {
                device.register_waker(waker);
            }
        }
    }
}
