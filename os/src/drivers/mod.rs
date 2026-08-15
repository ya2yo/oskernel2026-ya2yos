//! Common device interfaces and transport-independent drivers.
//!
//! Platform-specific transport bindings are kept under [`crate::arch`]. The
//! re-exports below preserve the existing kernel-facing `crate::drivers::*`
//! API while keeping architecture details out of this module.

pub mod devcont;
pub mod device;
pub mod disk;
#[cfg(feature = "net")]
pub mod net;
pub mod virtio;

pub use crate::arch::drivers::BlockDeviceImpl;
#[cfg(feature = "net")]
pub use crate::arch::drivers::NetDeviceImpl;

pub use devcont::*;
pub use device::*;
pub use disk::*;
#[cfg(feature = "net")]
pub use net::*;
pub use virtio::*;
