//! RISC-V VirtIO transport bindings.

mod blk;
pub use blk::VirtIoBlkDev;

use crate::arch::memory_layout::KERNEL_ADDR_OFFSET;
use crate::drivers::virtio::{VirtIoHalCMAImpl, VirtIoNetDev};
use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};

const QUEUE_SIZE: usize = 128;
const VIRTIO_BLK_BASE: usize = 0x10001000 + KERNEL_ADDR_OFFSET;

pub type BlockDeviceImpl = VirtIoBlkDev<VirtIoHalCMAImpl>;

#[cfg(feature = "net")]
const VIRTIO_NET_BASE: usize = 0x10008000 + KERNEL_ADDR_OFFSET;
#[cfg(feature = "net")]
pub type NetDeviceImpl = VirtIoNetDev<VirtIoHalCMAImpl, MmioTransport, QUEUE_SIZE>;

/// Translate a VirtIO MMIO physical address to the kernel virtual address
/// domain used by the generic HAL.
pub(crate) fn mmio_phys_addr(paddr: usize) -> usize {
    paddr
}

impl BlockDeviceImpl {
    pub fn new_device() -> Self {
        unsafe { VirtIoBlkDev::new(&mut *(VIRTIO_BLK_BASE as *mut VirtIOHeader)) }
    }
}

#[cfg(feature = "net")]
impl NetDeviceImpl {
    pub fn try_new_device() -> Option<Self> {
        use core::ptr::NonNull;
        use log::warn;

        let Some(header) = NonNull::new(VIRTIO_NET_BASE as *mut VirtIOHeader) else {
            warn!("VirtIO Net base address is null");
            return None;
        };

        let transport = match unsafe { MmioTransport::new(header) } {
            Ok(transport) => transport,
            Err(err) => {
                warn!(
                    "No usable VirtIO Net MMIO device at {:#x}: {:?}",
                    VIRTIO_NET_BASE, err
                );
                return None;
            }
        };

        match Self::try_new(transport, None) {
            Ok(device) => Some(device),
            Err(err) => {
                warn!("Failed to initialize VirtIO Net device: {:?}", err);
                None
            }
        }
    }

    pub fn new_device() -> Self {
        Self::try_new_device().expect("Failed to initialize VirtIO Net device")
    }
}
