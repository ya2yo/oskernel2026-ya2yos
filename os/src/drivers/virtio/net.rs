use super::VirtIoHalCMAImpl;
use virtio_drivers::{VirtIONet, Transport};
use alloc::vec::Vec;
use spin::Mutex;

/// 封装 VirtIO 网络设备
pub struct VirtIoNet<T: Transport> {
    inner: Mutex<VirtIONet<VirtIoHalCMAImpl, T>>,
}

impl<T: Transport> VirtIoNet<T> {
    /// 创建一个新的网卡驱动实例
    pub fn new(transport: T) -> Self {
        // 使用你现有的 HAL 实现来初始化网卡
        // 这里的 1514 是以太网最大帧长度
        let inner = VirtIONet::new(transport)
            .expect("failed to create virtio net driver");
        Self {
            inner: Mutex::new(inner),
        }
    }

    /// 获取 MAC 地址
    pub fn mac_address(&self) -> [u8; 6] {
        self.inner.lock().mac_address()
    }

    /// 发送数据包
    pub fn send(&self, data: &[u8]) {
        self.inner.lock().send(data).expect("virtio_net send error");
    }

    /// 接收数据包
    /// 返回接收到的字节数，如果没有数据则返回 0
    pub fn recv(&self, buf: &mut [u8]) -> usize {
        let mut inner = self.inner.lock();
        if let Ok(pkt) = inner.receive() {
            let len = pkt.len();
            buf[..len].copy_from_slice(pkt.packet());
            // 必须在处理完包后显式释放，以便让网卡重新使用该缓冲区
            inner.recycle_rx_buffer(pkt).expect("virtio_net recycle error");
            len
        } else {
            0
        }
    }

    /// 轮询状态（在你的 poll_interfaces 中调用）
    pub fn poll(&self) {
        // virtio-drivers 的一些版本可能需要显式 ack 某些操作
        // 或者在这里处理中断
    }
}