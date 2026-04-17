cfg_if::cfg_if! {
    if #[cfg(feature = "riscv64")] {
        mod riscv64;
        pub use riscv64::*;
    } else if #[cfg(feature = "loongarch64")] {
        mod loongarch64;
        pub use loongarch64::*;
    } else {
        mod dummy;
        pub use dummy::*;
    }
}
