//! VirtIO 网络设备驱动实现。
//!
//! 本模块将 `virtio-drivers` 提供的原始 VirtIO 网络设备封装为内核统一的
//! [`NetDriverOps`] 接口，负责初始化收发队列、管理网络缓冲区，并处理设备
//! 完成的收发请求。
//!
//! 接收方向为每个 VirtIO 描述符预先提交一个 [`NetBuf`]；收到数据后，驱动
//! 将缓冲区交给网络协议栈，协议栈使用完毕后通过
//! [`NetDriverOps::recycle_rx_buffer`] 将其重新提交给设备。发送方向从缓冲
//! 区池中取得空闲缓冲区，提交完成后由 [`NetDriverOps::recycle_tx_buffers`]
//! 回收。

use crate::drivers::{VirtError, VirtResult};
use alloc::{sync::Arc, vec::Vec};
use log::warn;

use super::super::{BaseDriver, DevError, DevResult, DeviceType};

use super::super::net::{EthernetAddress, NetBuf, NetBufBox, NetBufPool, NetBufPtr, NetDriverOps};
use virtio_drivers::{device::net::VirtIONetRaw as InnerDev, transport::Transport, Hal};

use super::as_dev_err;

/// The size of each network buffer, including the VirtIO net header area.
const NET_BUF_LEN: usize = 1526;
/// The largest Ethernet frame accepted by the network buffer interface.
const MAX_BUFFER_LEN: usize = 65535;
/// The smallest Ethernet frame size used by the network buffer interface.
const MIN_BUFFER_LEN: usize = 1526;

/// A VirtIO network device backed by one receive and one transmit queue.
///
/// The driver keeps one buffer associated with every receive descriptor and
/// takes ownership of a transmit buffer until the device reports completion.
/// `QS` is the number of descriptors in each VirtIO queue. `H` supplies the
/// DMA and memory-management operations required by `virtio-drivers`, while
/// `T` identifies the platform-specific VirtIO transport.
///
/// Receive buffers are returned to the device by [`Self::recycle_rx_buffer`]
/// after the network stack has finished using them. Transmit buffers follow a
/// similar lifecycle: [`Self::alloc_tx_buffer`] removes one from the free
/// list, [`Self::transmit`] submits it, and [`Self::recycle_tx_buffers`] puts
/// it back after completion.
pub struct VirtIoNetDev<H: Hal, T: Transport, const QS: usize> {
    /// Buffers currently posted to the receive virtqueue, indexed by token.
    rx_buffers: [Option<NetBufBox>; QS],
    /// Buffers submitted to the transmit virtqueue, indexed by token.
    tx_buffers: [Option<NetBufBox>; QS],
    /// Transmit buffers that are not currently owned by the device.
    free_tx_bufs: Vec<NetBufBox>,
    /// Shared allocator backing all receive and transmit buffers.
    buf_pool: Arc<NetBufPool>,
    /// The transport-specific VirtIO network device implementation.
    inner: InnerDev<H, T, QS>,
    /// Interrupt number associated with the device, when one is configured.
    irq: Option<usize>,
}

// SAFETY: NetBuf and the underlying VirtIO device are designed to be moved
// between the contexts that own the driver; queue access is serialized by the
// network-driver layer.
unsafe impl<H: Hal, T: Transport, const QS: usize> Send for VirtIoNetDev<H, T, QS> {}
// SAFETY: Shared access is exposed only through the driver interfaces, whose
// callers provide the required synchronization around mutable queue access.
unsafe impl<H: Hal, T: Transport, const QS: usize> Sync for VirtIoNetDev<H, T, QS> {}

impl<H: Hal, T: Transport, const QS: usize> VirtIoNetDev<H, T, QS> {
    /// Converts a VirtIO descriptor token into a queue-array index.
    ///
    /// A token outside the configured queue range indicates corrupted device
    /// state and is rejected instead of indexing the buffer arrays.
    fn token_index(token: u16) -> DevResult<usize> {
        let idx = token as usize;
        if idx >= QS {
            warn!("invalid virtqueue token {token} (queue size {QS})");
            Err(DevError::BadState)
        } else {
            Ok(idx)
        }
    }

    /// Creates and initializes a VirtIO network device.
    ///
    /// The constructor allocates a shared pool containing enough buffers for
    /// all receive and transmit descriptors, posts every receive buffer to the
    /// device, and prepares every transmit buffer with a VirtIO network header.
    ///
    /// `irq` is retained as the device's optional interrupt number; interrupt
    /// registration itself is performed by the caller or platform layer.
    ///
    /// # Errors
    ///
    /// Returns an error if the transport cannot initialize the device, the
    /// buffer pool cannot be allocated, or a VirtIO network header cannot be
    /// prepared.
    pub fn try_new(transport: T, irq: Option<usize>) -> DevResult<Self> {
        // 0. Create a new driver instance.
        const NONE_BUF: Option<NetBufBox> = None;
        let inner = InnerDev::new(transport).map_err(as_dev_err)?;
        let rx_buffers = [NONE_BUF; QS];
        let tx_buffers = [NONE_BUF; QS];
        let buf_pool = NetBufPool::new(2 * QS, NET_BUF_LEN)?;
        let free_tx_bufs = Vec::with_capacity(QS);

        let mut dev = Self {
            rx_buffers,
            inner,
            tx_buffers,
            free_tx_bufs,
            buf_pool,
            irq,
        };

        // 1. Fill all rx buffers.
        for (i, rx_buf_place) in dev.rx_buffers.iter_mut().enumerate() {
            let mut rx_buf = dev.buf_pool.alloc_boxed().ok_or(DevError::NoMemory)?;
            // Safe because the buffer lives as long as the queue.

            let token = unsafe {
                dev.inner
                    .receive_begin(rx_buf.raw_buf_mut())
                    .map_err(as_dev_err)?
            };
            assert_eq!(token, i as u16);
            *rx_buf_place = Some(rx_buf);
        }

        // 2. Allocate all tx buffers.
        for _ in 0..QS {
            let mut tx_buf = dev.buf_pool.alloc_boxed().ok_or(DevError::NoMemory)?;
            // Fill header
            let hdr_len = dev
                .inner
                .fill_buffer_header(tx_buf.raw_buf_mut())
                .or(Err(DevError::InvalidParam))?;
            tx_buf.set_header_len(hdr_len);
            dev.free_tx_bufs.push(tx_buf);
        }

        // 3. Return the driver instance.
        Ok(dev)
    }
}

impl<H: Hal, T: Transport, const QS: usize> BaseDriver for VirtIoNetDev<H, T, QS> {
    fn device_name(&self) -> &str {
        "virtio-net"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Net
    }

    fn irq_num(&self) -> Option<usize> {
        self.irq
    }
}

impl<H: Hal, T: Transport, const QS: usize> NetDriverOps for VirtIoNetDev<H, T, QS> {
    #[inline]
    fn mac_address(&self) -> EthernetAddress {
        EthernetAddress(self.inner.mac_address())
    }

    #[inline]
    fn can_transmit(&self) -> bool {
        !self.free_tx_bufs.is_empty() && self.inner.can_send()
    }

    #[inline]
    fn can_receive(&self) -> bool {
        self.inner.poll_receive().is_some()
    }

    #[inline]
    fn rx_queue_size(&self) -> usize {
        QS
    }

    #[inline]
    fn tx_queue_size(&self) -> usize {
        QS
    }

    fn recycle_rx_buffer(&mut self, rx_buf: NetBufPtr) -> DevResult {
        let mut rx_buf = unsafe { NetBuf::from_buf_ptr(rx_buf) };
        // Safe because we take the ownership of `rx_buf` back to `rx_buffers`,
        // it lives as long as the queue.
        let new_token = unsafe {
            self.inner
                .receive_begin(rx_buf.raw_buf_mut())
                .map_err(as_dev_err)?
        };
        // `rx_buffers[new_token]` is expected to be `None` since it was taken
        // away at `Self::receive()` and has not been added back.
        let idx = Self::token_index(new_token)?;
        if self.rx_buffers[idx].is_some() {
            return Err(DevError::BadState);
        }
        self.rx_buffers[idx] = Some(rx_buf);
        Ok(())
    }

    fn recycle_tx_buffers(&mut self) -> DevResult {
        while let Some(token) = self.inner.poll_transmit() {
            let idx = Self::token_index(token)?;
            let tx_buf = self.tx_buffers[idx].take().ok_or(DevError::BadState)?;
            unsafe {
                self.inner
                    .transmit_complete(token, tx_buf.packet_with_header())
                    .map_err(as_dev_err)?;
            }
            // Recycle the buffer.
            self.free_tx_bufs.push(tx_buf);
        }
        Ok(())
    }

    fn transmit(&mut self, tx_buf: NetBufPtr) -> DevResult {
        // 0. prepare tx buffer.
        let tx_buf = unsafe { NetBuf::from_buf_ptr(tx_buf) };
        // 1. transmit packet.
        let token = unsafe {
            self.inner
                .transmit_begin(tx_buf.packet_with_header())
                .map_err(as_dev_err)?
        };
        let idx = Self::token_index(token)?;
        self.tx_buffers[idx] = Some(tx_buf);
        Ok(())
    }

    fn receive(&mut self) -> DevResult<NetBufPtr> {
        self.inner.ack_interrupt();
        if let Some(token) = self.inner.poll_receive() {
            let idx = Self::token_index(token)?;
            let mut rx_buf = self.rx_buffers[idx].take().ok_or(DevError::BadState)?;
            // Safe because the buffer lives as long as the queue.
            let (hdr_len, pkt_len) = unsafe {
                self.inner
                    .receive_complete(token, rx_buf.raw_buf_mut())
                    .map_err(as_dev_err)?
            };
            rx_buf.set_header_len(hdr_len);
            rx_buf.set_packet_len(pkt_len);

            Ok(rx_buf.into_buf_ptr())
        } else {
            Err(DevError::Again)
        }
    }

    fn alloc_tx_buffer(&mut self, size: usize) -> DevResult<NetBufPtr> {
        // 0. Allocate a buffer from the queue.
        let mut net_buf = self.free_tx_bufs.pop().ok_or(DevError::NoMemory)?;
        let pkt_len = size;

        // 1. Check if the buffer is large enough.
        let hdr_len = net_buf.header_len();
        if hdr_len + pkt_len > net_buf.capacity() {
            return Err(DevError::InvalidParam);
        }
        net_buf.set_packet_len(pkt_len);

        // 2. Return the buffer.
        Ok(net_buf.into_buf_ptr())
    }
}
