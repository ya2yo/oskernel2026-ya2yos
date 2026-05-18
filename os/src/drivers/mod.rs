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
use spin::Lazy;
pub use virtio::*;
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

pub type NetDeviceImpl = VirtIoNetDev<VirtIoHalCMAImpl, MmioTransport, QUEUE_SIZE>;

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
    pub fn new_device() -> Self {
        use core::ptr::NonNull;
        let header = NonNull::new(VIRTIO_NET_BASE as *mut VirtIOHeader)
            .expect("VirtIO Net base address is null");
        let transport = unsafe {
            MmioTransport::new(header).expect("Failed to create MmioTransport for VirtIO Net")
        };
        Self::try_new(transport, None).expect("Failed to initialize VirtIoNetDev")
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn new_device() -> Self {
        use core::ptr::NonNull;
        const VIRTIO_NET_BASE: usize = 0x1fe00000;
        let header = NonNull::new(VIRTIO_NET_BASE as *mut VirtIOHeader)
            .expect("VirtIO Net base address is null");
        let transport = unsafe {
            MmioTransport::new(header).expect("Failed to create MmioTransport for VirtIO Net");
        };
        Self::try_new(transport, None).expect("Failed to initialize VirtIoNetDev")
    }
}
