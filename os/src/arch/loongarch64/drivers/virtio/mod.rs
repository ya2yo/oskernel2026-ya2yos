//! LoongArch VirtIO PCI transport bindings.

mod pci;
pub use pci::VirtIoBlkDev2;

use crate::arch::memory_layout::{VIRTIO_PCI_MMIO_BASE, VIRTIO_PCI_MMIO_SIZE};
use crate::drivers::virtio::{VirtIoHalCMAImpl, VirtIoNetDev};
#[cfg(feature = "net")]
use virtio_drivers::transport::pci::PciTransport;

const QUEUE_SIZE: usize = 128;

pub type BlockDeviceImpl = VirtIoBlkDev2<VirtIoHalCMAImpl>;

#[cfg(feature = "net")]
pub type NetDeviceImpl = VirtIoNetDev<VirtIoHalCMAImpl, PciTransport, QUEUE_SIZE>;

/// Normalize a PCI BAR address before the generic HAL adds the kernel offset.
pub(crate) fn mmio_phys_addr(paddr: usize) -> usize {
    if paddr >= VIRTIO_PCI_MMIO_BASE && paddr < VIRTIO_PCI_MMIO_BASE + VIRTIO_PCI_MMIO_SIZE {
        return paddr;
    }
    if paddr >= 0x1_0000_0000_0000 {
        let lo = paddr as u32 as usize;
        if lo < VIRTIO_PCI_MMIO_SIZE {
            return VIRTIO_PCI_MMIO_BASE + lo;
        }
    }
    paddr
}

impl BlockDeviceImpl {
    pub fn new_device() -> Self {
        Self::new()
    }
}

#[cfg(feature = "net")]
impl NetDeviceImpl {
    pub fn new_device() -> Self {
        let transport = pci::create_net_transport();
        Self::try_new(transport, None).expect("Failed to initialize VirtIoNetDev")
    }
}
