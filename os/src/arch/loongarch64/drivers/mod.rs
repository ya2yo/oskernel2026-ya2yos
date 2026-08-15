//! LoongArch platform drivers.
//!
//! Device interfaces and transport-independent implementations live in
//! [`crate::drivers`]. This module contains the LoongArch PCI transport
//! bindings and platform MMIO address handling.

#[cfg(feature = "2k1000")]
mod ahci;
#[cfg(all(feature = "net", feature = "2k1000"))]
mod ls2k1000_gmac;
pub mod virtio;

#[cfg(feature = "2k1000")]
pub use ahci::AhciBlkDev;
#[cfg(feature = "2k1000")]
pub type BlockDeviceImpl = AhciBlkDev;
#[cfg(not(feature = "2k1000"))]
pub use virtio::BlockDeviceImpl;
#[cfg(all(feature = "net", feature = "2k1000"))]
pub use ls2k1000_gmac::Ls2k1000Gmac as NetDeviceImpl;
#[cfg(all(feature = "net", not(feature = "2k1000")))]
pub use virtio::NetDeviceImpl;
