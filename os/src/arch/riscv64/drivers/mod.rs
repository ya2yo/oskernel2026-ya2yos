//! RISC-V platform drivers.
//!
//! Device interfaces and transport-independent implementations live in
//! [`crate::drivers`]. This module contains the RISC-V transport bindings and
//! startup addresses needed to instantiate those drivers.

pub mod virtio;

#[cfg(all(feature = "net", feature = "visionfive2"))]
pub mod visionfive2;

pub use virtio::BlockDeviceImpl;
#[cfg(all(feature = "net", not(feature = "visionfive2")))]
pub use virtio::NetDeviceImpl;
#[cfg(all(feature = "net", feature = "visionfive2"))]
pub use visionfive2::VisionFive2Gmac as NetDeviceImpl;
