use crate::drivers::{BaseDriver, BlockDriver, DevResult, DeviceType};
use spin::Mutex;
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::mmio::MmioTransport;
use virtio_drivers::transport::mmio::VirtIOHeader;
use virtio_drivers::Hal;

use super::as_dev_err;

pub struct VirtIoBlkDev<H: Hal> {
    inner: Mutex<VirtIOBlk<H, MmioTransport>>,
}

unsafe impl<H: Hal> Send for VirtIoBlkDev<H> {}
unsafe impl<H: Hal> Sync for VirtIoBlkDev<H> {}

impl<H: Hal> VirtIoBlkDev<H> {
    pub fn new(header: &'static mut VirtIOHeader) -> Self {
        let transport: MmioTransport = unsafe { MmioTransport::new(header.into()).unwrap() };
        Self {
            inner: Mutex::new(VirtIOBlk::new(transport).expect("VirtIOBlk create failed")),
        }
    }
}

impl<H: Hal> BaseDriver for VirtIoBlkDev<H> {
    fn device_name(&self) -> &str {
        "virtio-blk"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }
}

impl<H: Hal> BlockDriver for VirtIoBlkDev<H> {
    #[inline]
    fn num_blocks(&self) -> usize {
        self.inner.lock().capacity() as usize
    }

    #[inline]
    fn block_size(&self) -> usize {
        512
    }

    fn read_block(&mut self, block_id: usize, buf: &mut [u8]) -> DevResult {
        self.inner
            .lock()
            .read_block(block_id as _, buf)
            .map_err(as_dev_err)
    }

    fn write_block(&mut self, block_id: usize, buf: &[u8]) -> DevResult {
        self.inner
            .lock()
            .write_block(block_id as _, buf)
            .map_err(as_dev_err)
    }

    fn flush(&mut self) -> DevResult {
        Ok(())
    }
}
