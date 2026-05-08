mod device;
mod disk;
mod virtio;
mod devcont;
#[cfg(feature = "net")]
mod net;
pub use device::*;
pub use disk::*;
pub use virtio::*;
pub use devcont::*;
#[cfg(feature = "net")]
pub use net::*;
use virtio_drivers::{device::net::VirtIONet, transport::mmio::{MmioTransport, VirtIOHeader}};

use crate::arch::memory_layout::KERNEL_ADDR_OFFSET;

#[cfg(feature = "riscv64")]
pub type BlockDeviceImpl = VirtIoBlkDev<VirtIoHalCMAImpl>;

#[cfg(feature = "loongarch64")]
pub type BlockDeviceImpl = VirtIoBlkDev2<VirtIoHalCMAImpl>;
pub type NetDeviceImpl=VirtIONet<VirtIoHalCMAImpl, MmioTransport>;
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
