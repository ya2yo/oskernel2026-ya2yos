//! RISC-V page geometry shared by every supported platform.
//!
//! QEMU `virt` and VisionFive 2 share this configuration while their
//! platform-specific layout, boot, MMIO, and driver definitions remain local.

/// Base page size selected by the kernel's RISC-V page-table configuration.
pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = 12;

/// User hugetlb mappings use the Sv39 level-1 2 MiB leaf size.
pub const HUGE_PAGE_SIZE: usize = 0x20_0000;
pub const HUGE_PAGE_PAGES: usize = HUGE_PAGE_SIZE / PAGE_SIZE;
