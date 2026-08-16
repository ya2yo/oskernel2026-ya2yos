// 定义内存布局相关常量，并且定义了MMIO区域

use crate::config::THREAD_MAX_NUM;

// The boot page table in entry.asm has a single 1GiB direct-map leaf. CMA
// must not place its free-list metadata above this range before the full
// kernel page table is active.
pub const BOOTSTRAP_PHYSICAL_MEMORY_SIZE: usize = 0x4000_0000; // 1GB

pub const PAGE_SIZE: usize = 0x1000; // 4KB
pub const PAGE_SIZE_BITS: usize = 12;
/// User hugetlb mappings use the Sv39 2 MiB leaf size.
pub const HUGE_PAGE_SIZE: usize = 0x20_0000;
pub const HUGE_PAGE_PAGES: usize = HUGE_PAGE_SIZE / PAGE_SIZE;

pub const USER_STACK_SIZE: usize = 1024 * 1024 * 8; // 8MB
pub const KERNEL_STACK_SIZE: usize = PAGE_SIZE * 4;
// Full pre-test runs retain kernel objects while execing the 1.7 MiB glibc
// busybox image. Keep one 2 MiB buddy block available for that normal load.
pub const KERNEL_HEAP_SIZE: usize = 0x8_000_000; // 128MB
                                                 // Rustc's linker-stage workers can grow the process brk beyond 512 MiB.
                                                 // Keep the virtual reservation aligned with the lazy mmap budget; pages remain
                                                 // demand-allocated, so this does not reserve physical memory up front.
pub const USER_HEAP_SIZE: usize = 0x8000_0000; // 2 GiB (virtual reservation)
/// Maximum heap (brk) growth per process.
/// Caps runaway brk from exhausting physical memory while leaving room for
/// the Rust toolchain's normal peak working set.
pub const MAX_BRK_SIZE: usize = 0x8000_0000; // 2 GiB
/// Maximum total lazy mmap virtual reservation per process.
/// Rustc reserves several 128 MiB PROT_NONE arenas before those pages are
/// faulted in, so this must leave room beyond the physical-memory working set
/// while still bounding VMA growth.
pub const MAX_MMAP_SIZE: usize = 0x8000_0000; // 2 GiB
pub const PRE_ALLOC_PAGES: usize = 8;

#[cfg(feature = "visionfive2")]
pub const KERNEL_ADDR_OFFSET: usize = 0xffff_ffc0_4000_0000;
#[cfg(not(feature = "visionfive2"))]
pub const KERNEL_ADDR_OFFSET: usize = 0xffff_ffc0_0000_0000;

/// When directly map: vpn = ppn + kernel direct offset
pub const KERNEL_PGNUM_OFFSET: usize = KERNEL_ADDR_OFFSET >> PAGE_SIZE_BITS;

pub const USER_SPACE_SIZE: usize = 0x30_0000_0000;

/// User Space layout
/// TrapContext GuardPage Stack GuardPage Mmap Heap Elf
pub const USER_TRAP_CONTEXT_TOP: usize = USER_SPACE_SIZE;
pub const USER_STACK_TOP: usize = USER_TRAP_CONTEXT_TOP - PAGE_SIZE * THREAD_MAX_NUM;
pub const MMAP_TOP: usize = USER_TRAP_CONTEXT_TOP
    - PAGE_SIZE * THREAD_MAX_NUM
    - USER_STACK_SIZE * THREAD_MAX_NUM
    - PAGE_SIZE;

/// Dynamic linked interpreter address range in user space
pub const DL_INTERP_OFFSET: usize = 0x15_0000_0000;
/// Kernel Stack Start
pub const KSTACK_TOP: usize = usize::MAX - PAGE_SIZE + 1;

pub fn memory_end() -> usize {
    crate::arch::hardware::ram_start() + crate::arch::hardware::ram_size() + KERNEL_ADDR_OFFSET
}

pub const MMIO: &[(usize, usize)] = &[
    (0x0010_0000, 0x00_1000), // VIRT_TEST
    (0x0010_1000, 0x00_1000), // VIRT_RTC
    (0x1000_0000, 0x00_1000), // UART0
    (0x1000_1000, 0x00_1000), // Virtio Block
    (0x1000_8000, 0x00_1000), // Virtio Net
];

#[cfg(feature = "visionfive2")]
pub const BOARD_MMIO: &[(usize, usize)] = &[
    (0x1602_0000, 0x01_0000), // JH7110 SD/MMC
    (0x1604_0000, 0x01_0000), // JH7110 GMAC
    (0x1302_0000, 0x00_1000), // JH7110 clock/reset controller
    (0x1303_0000, 0x00_1000), // JH7110 syscon
    (0x1600_0000, 0x00_1000), // JH7110 GPIO
];

pub const MMIO_MAP_OFFSET: usize = KERNEL_ADDR_OFFSET;

extern "C" {
    pub fn sigreturn_trampoline();
}

pub fn sigreturn_ka() -> usize {
    sigreturn_trampoline as *const () as usize
}

pub fn sigreturn_pa() -> usize {
    sigreturn_trampoline as *const () as usize - KERNEL_ADDR_OFFSET
}

pub fn sigreturn_va() -> usize {
    sigreturn_trampoline as *const () as usize
}
