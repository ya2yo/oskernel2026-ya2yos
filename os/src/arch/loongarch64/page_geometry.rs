//! LoongArch page geometry shared by every supported platform.
//!
//! Platform memory layouts retain board-specific addresses and capacity limits,
//! then re-export these constants through `crate::arch::memory_layout`.

/// Base page size selected by the kernel's LoongArch page-table configuration.
pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = 12;

/// User hugetlb mappings use the LoongArch level-1 2 MiB leaf size.
pub const HUGE_PAGE_SIZE: usize = 0x20_0000;
pub const HUGE_PAGE_PAGES: usize = HUGE_PAGE_SIZE / PAGE_SIZE;
