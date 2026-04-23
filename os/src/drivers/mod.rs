mod device;
mod disk;
mod virtio;
mod devcont;
mod net;
pub use device::*;
pub use disk::*;
pub use virtio::*;
pub use devcont::*;
pub use net::*;
use virtio_drivers::transport::mmio::VirtIOHeader;

use crate::arch::memory_layout::KERNEL_ADDR_OFFSET;

#[cfg(feature = "riscv64")]
pub type BlockDeviceImpl = VirtIoBlkDev<VirtIoHalCMAImpl>;

#[cfg(feature = "loongarch64")]
pub type BlockDeviceImpl = VirtIoBlkDev2<VirtIoHalCMAImpl>;

impl BlockDeviceImpl {
    #[cfg(feature = "riscv64")]
    pub fn new_device() -> Self {
        const VIRTIO0: usize = 0x10001000 + KERNEL_ADDR_OFFSET;
        unsafe { VirtIoBlkDev::new(&mut *(VIRTIO0 as *mut VirtIOHeader)) }
    }

    #[cfg(feature = "loongarch64")]
    pub fn new_device() -> Self {
        Self::new()
    }
}
