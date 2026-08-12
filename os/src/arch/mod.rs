pub mod hardware;

cfg_if::cfg_if! {
    if #[cfg(target_arch = "riscv64")] {
        mod riscv64;
        pub use riscv64::*;
    } else if #[cfg(target_arch = "loongarch64")] {
        mod loongarch64;
        pub use loongarch64::*;
    }
}
// 中断请求
mod irq;
pub use irq::*;

use crate::signal::SigSet;
pub const PADDING_SIZE: usize = 128;
pub const __PAD_SIZE: usize = PADDING_SIZE - core::mem::size_of::<SigSet>();
