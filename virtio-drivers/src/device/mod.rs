//! Drivers for specific VirtIO devices.

pub mod blk;
pub mod console;
pub mod gpu;
#[cfg(feature = "alloc")]
pub mod input;
pub mod net;
const MAX_BUFFER_LEN: usize = 65535;
const MIN_BUFFER_LEN: usize = 1526;

/// The type returned by driver methods.
pub type VirtResult<T = ()> = core::result::Result<T, VirtError>;

/// The error type of VirtIO drivers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VirtError {
    /// There are not enough descriptors available in the virtqueue, try again later.
    QueueFull,
    /// The device is not ready.
    NotReady,
    /// The device used a different descriptor chain to the one we were expecting.
    WrongToken,
    /// The queue is already in use.
    AlreadyUsed,
    /// Invalid parameter.
    InvalidParam,
    /// Failed to alloc DMA memory.
    DmaError,
    /// I/O Error
    IoError,
    /// The request was not supported by the device.
    Unsupported,
    /// The config space advertised by the device is smaller than the driver expected.
    ConfigSpaceTooSmall,
    /// The device doesn't have any config space, but the driver expects some.
    ConfigSpaceMissing,
}

