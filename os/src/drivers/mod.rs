mod devcont;
mod device;
mod disk;
#[cfg(feature = "net")]
mod net;
mod virtio;
use alloc::sync::Arc;
pub use devcont::*;
pub use device::*;
pub use disk::*;
#[cfg(feature = "net")]
pub use net::*;
#[cfg(feature = "net")]
use log::warn;
use spin::Lazy;
pub use virtio::*;
#[cfg(target_arch = "loongarch64")]
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::{
    device::net::VirtIONet,
    transport::mmio::{MmioTransport, VirtIOHeader},
};

use crate::arch::memory_layout::KERNEL_ADDR_OFFSET;
const QUEUE_SIZE: usize = 128;

#[cfg(target_arch = "riscv64")]
pub type BlockDeviceImpl = VirtIoBlkDev<VirtIoHalCMAImpl>;

#[cfg(target_arch = "loongarch64")]
pub type BlockDeviceImpl = VirtIoBlkDev2<VirtIoHalCMAImpl>;

#[cfg(target_arch = "riscv64")]
const VIRTIO_BLK_BASE: usize = 0x10001000 + KERNEL_ADDR_OFFSET;
#[cfg(target_arch = "riscv64")]
const VIRTIO_NET_BASE: usize = 0x10002000 + KERNEL_ADDR_OFFSET;
#[cfg(target_arch = "riscv64")]
pub type NetDeviceImpl = VirtIoNetDev<VirtIoHalCMAImpl, MmioTransport, QUEUE_SIZE>;
#[cfg(target_arch = "loongarch64")]
pub type NetDeviceImpl = VirtIoNetDev<VirtIoHalCMAImpl, PciTransport, QUEUE_SIZE>;

impl BlockDeviceImpl {
    #[cfg(target_arch = "riscv64")]
    pub fn new_device() -> Self {
        const VIRTIO0: usize = 0x10001000 + KERNEL_ADDR_OFFSET;
        unsafe { VirtIoBlkDev::new(&mut *(VIRTIO0 as *mut VirtIOHeader)) }
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn new_device() -> Self {
        Self::new()
    }
}

impl NetDeviceImpl {
    #[cfg(target_arch = "riscv64")]
    pub fn try_new_device() -> Option<Self> {
        use core::ptr::NonNull;

        let Some(header) = NonNull::new(VIRTIO_NET_BASE as *mut VirtIOHeader) else {
            warn!("VirtIO Net base address is null");
            return None;
        };

        let transport = match unsafe { MmioTransport::new(header) } {
            Ok(transport) => transport,
            Err(err) => {
                warn!("No usable VirtIO Net MMIO device at {:#x}: {:?}", VIRTIO_NET_BASE, err);
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

    #[cfg(target_arch = "riscv64")]
    pub fn new_device() -> Self {
        Self::try_new_device().expect("Failed to initialize VirtIO Net device")
    }

    #[cfg(all(target_arch = "loongarch64", feature = "net"))]
    pub fn new_device() -> Self {
        let transport = crate::drivers::virtio::loongarch::pci::create_net_transport();
        Self::try_new(transport, None).expect("Failed to initialize VirtIoNetDev")
    }
}
