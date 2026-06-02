// 定义内存布局相关常量，并且定义了MMIO区域

use crate::config::THREAD_MAX_NUM;

// 物理内存的起始地址
pub const PHYSICAL_MEMORY_START: usize = 0x8000_0000;
// 物理内存大小 TODO:修改了qemu MEM参数，也要修改这里
pub const PHYSICAL_MEMORY_SIZE: usize = 0x800_0000; //128MB

pub const PAGE_SIZE: usize = 0x1000; // 4KB
pub const PAGE_SIZE_BITS: usize = 12;

pub const USER_STACK_SIZE: usize = 1024 * 1024 * 8; // 8MB
pub const KERNEL_STACK_SIZE: usize = PAGE_SIZE * 4;
pub const KERNEL_HEAP_SIZE: usize = 0x3_000_000; // 48MB
pub const USER_HEAP_SIZE: usize = 0x10_000_000; // 256MB
/// Maximum total mmap size per process.
/// Prevent runaway mmap from consuming all virtual space
/// and subsequently exhausting physical memory via lazy page faults.
pub const MAX_MMAP_SIZE: usize = 0x100_000_00; // 256MB
pub const PRE_ALLOC_PAGES: usize = 8;

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

// 内核虚拟地址空间中对应的内存结束地址
pub const MEMORY_END: usize = PHYSICAL_MEMORY_START + PHYSICAL_MEMORY_SIZE + KERNEL_ADDR_OFFSET;

pub const MMIO: &[(usize, usize)] = &[
    (0x0010_0000, 0x00_1000), // VIRT_TEST
    (0x0010_1000, 0x00_1000), // VIRT_RTC
    (0x1000_0000, 0x00_1000), // UART0
    (0x1000_1000, 0x00_1000), // Virtio Block
    (0x1000_2000, 0x00_1000), // Virtio Net
];

pub const MMIO_MAP_OFFSET: usize = KERNEL_ADDR_OFFSET;

extern "C" {
    fn sigreturn_trampoline();
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
