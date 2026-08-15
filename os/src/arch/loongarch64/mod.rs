#[cfg(feature = "2k1000")]
mod board_2k1000;
#[cfg(not(feature = "2k1000"))]
mod qemu;

#[cfg(feature = "2k1000")]
pub use board_2k1000::*;
#[cfg(not(feature = "2k1000"))]
pub use qemu::*;
pub mod drivers;
