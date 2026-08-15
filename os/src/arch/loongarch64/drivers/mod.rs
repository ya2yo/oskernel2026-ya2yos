//! LoongArch platform drivers.
//!
//! Device interfaces and transport-independent implementations live in
//! [`crate::drivers`]. This module contains the LoongArch PCI transport
//! bindings and platform MMIO address handling.

pub mod virtio;

pub use virtio::BlockDeviceImpl;
#[cfg(feature = "net")]
pub use virtio::NetDeviceImpl;
