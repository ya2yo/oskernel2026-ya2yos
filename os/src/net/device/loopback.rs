use alloc::vec;
use core::task::Waker;
use log::warn;

use crate::utils::PollSet;
use smoltcp::{
    storage::{PacketBuffer, PacketMetadata},
    time::Instant,
    wire::IpAddress,
};

use super::super::{
    consts::{SOCKET_BUFFER_SIZE, STANDARD_MTU},
    device::Device,
};
/// 本地回环设备结构体
/// 在内存中模拟了一个网络接口，发送的数据包会直接存入内部缓冲区，供接收逻辑读取
pub struct LoopbackDevice {
    /// 内部数据包缓冲区
    /// 由于是回环设备，发送和接收共享同一个 FIFO 队列
    buffer: PacketBuffer<'static, ()>,
    /// 唤醒器集合，用于管理和触发异步任务的 Waker
    poll: PollSet,
}
impl LoopbackDevice {
    /// 创建一个新的回环设备
    pub fn new() -> Self {
        // 初始化数据包缓冲区
        // PacketMetadata::EMPTY 用于存储数据包的元数据（如长度、位置）
        // 缓冲区总大小 = 标准 MTU * 最大数据包数量
        let buffer = PacketBuffer::new(
            vec![PacketMetadata::EMPTY; SOCKET_BUFFER_SIZE],
            vec![0u8; STANDARD_MTU * SOCKET_BUFFER_SIZE],
        );
        Self {
            buffer,
            poll: PollSet::new(),
        }
    }
}
/// 为回环设备实现统一的 Device 接口
impl Device for LoopbackDevice {
    fn name(&self) -> &str {
        "lo"
    }
    /// 接收数据包
    /// 尝试从内部缓冲区取出（dequeue）一个包，并拷贝到传入的 buffer 中
    fn recv(&mut self, buffer: &mut PacketBuffer<()>, _timestamp: Instant) -> bool {
        self.buffer.dequeue().ok().is_some_and(|(_, rx_buf)| {
            // 将数据从回环队列拷贝到协议栈提供的缓冲区
            buffer
                .enqueue(rx_buf.len(), ())
                .unwrap()
                .copy_from_slice(rx_buf);
            true
        })
    }
    /// 发送数据包
    /// 回环设备的发送就是直接把数据包塞进接收队列
    fn send(&mut self, next_hop: IpAddress, packet: &[u8], _timestamp: Instant) -> bool {
        match self.buffer.enqueue(packet.len(), ()) {
            Ok(tx_buf) => {
                tx_buf.copy_from_slice(packet);
                self.poll.wake();
                true
            }
            Err(_) => {
                warn!(
                    "Loopback device buffer is full, dropping packet to {}",
                    next_hop
                );
                false
            }
        }
    }
    /// 注册异步唤醒器
    /// 当协议栈（或 Executor）在等待网络事件时，会通过此函数注册 Waker
    fn register_waker(&self, waker: &Waker) {
        self.poll.register(waker);
    }
}
