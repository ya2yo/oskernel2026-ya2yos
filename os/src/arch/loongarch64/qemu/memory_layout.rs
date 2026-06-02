// 定义内存布局相关常量，并且定义了MMIO区域

use crate::config::THREAD_MAX_NUM;

pub const PHYSICAL_MEMORY_START: usize = 0; // la64的物理内存从0开始，而riscv的物理内存从0x8000_0000开始
pub const PHYSICAL_MEMORY_SIZE: usize = 0x800_0000; // 128MB

pub const PAGE_SIZE: usize = 0x1000; // 4KB
pub const PAGE_SIZE_BITS: usize = 12;

pub const USER_STACK_SIZE: usize = 1024 * 1024 * 8; // 8MB
pub const KERNEL_STACK_SIZE: usize = PAGE_SIZE * 2;
pub const KERNEL_HEAP_SIZE: usize = 0x3_000_000; // 48MB
pub const USER_HEAP_SIZE: usize = 0x10_000_000; // 256MB
/// Maximum total mmap size per process.
/// Prevent runaway mmap from consuming all virtual space
/// and subsequently exhausting physical memory via lazy page faults.
pub const MAX_MMAP_SIZE: usize = 0x100_000_00; // 256MB
pub const PRE_ALLOC_PAGES: usize = 8;

// 内核虚拟地址的起始地址
// pub const KERNEL_ADDR_OFFSET: usize = 0xFFFF_8000_0000_0000;
pub const KERNEL_ADDR_OFFSET: usize = 0x9000_0000_0000_0000;

/// When directly map: vpn = ppn + kernel direct offset
/// 这个值用于计算物理页号与内核地址之间的偏移关系
pub const KERNEL_PGNUM_OFFSET: usize = KERNEL_ADDR_OFFSET >> PAGE_SIZE_BITS;

// User space size (simplified)
pub const USER_SPACE_SIZE: usize = 0x30_0000_0000;

// User memory layout
pub const USER_TRAP_CONTEXT_TOP: usize = USER_SPACE_SIZE;
pub const USER_STACK_TOP: usize = USER_TRAP_CONTEXT_TOP - PAGE_SIZE * THREAD_MAX_NUM;
pub const MMAP_TOP: usize = USER_TRAP_CONTEXT_TOP
    - PAGE_SIZE * THREAD_MAX_NUM
    - USER_STACK_SIZE * THREAD_MAX_NUM
    - PAGE_SIZE;

// Dynamic linker interpreter base
pub const DL_INTERP_OFFSET: usize = 0x15_0000_0000;

// Kernel stack top (high address)
pub const KSTACK_TOP: usize = usize::MAX - PAGE_SIZE + 1;

// 内核虚拟地址空间中对应的内存结束地址
pub const MEMORY_END: usize = KERNEL_ADDR_OFFSET + PHYSICAL_MEMORY_START + PHYSICAL_MEMORY_SIZE;

// la64的MMIO相关
// 当entry.asm中启用了la64CPU的0x9000_...的直接映射窗口后，物理地址0x_0000_xxxx_xxxx_xxxx将被映射到虚拟地址0x9000_xxxx_xxxx_xxxx
// 所以下面的MMIO地址都是直接映射的虚拟地址

// 设备树定义串口设备地址
pub const UART_ADDR: usize = 0x1FE0_01E0;
// 设备树中定义的关机地址
pub const POWER_OFF_ADDR: usize = 0x100e_001c;

pub const MMIO_MAP_OFFSET: usize = 0xFFFF_FFFF_0000_0000;

// PCI MMIO window for virtio devices (physical addresses)
pub const VIRTIO_PCI_MMIO_BASE: usize = 0x4000_0000;
pub const VIRTIO_PCI_MMIO_SIZE: usize = 0x10_0000; // 1MB，覆盖 20 位 BAR 内偏移

// MMIO regions (physical addresses)
pub const MMIO: &[(usize, usize)] = &[
    (0x1fe0_0000, 0x1000),     // UART0
    (0x100e_0000, 0x1000),     // power off
    (0x2000_0000, 0x10000000), // PCI
];

pub fn print_memlayout() {
    extern "C" {
        fn ekernel();
    }
    println!("===MEMLAYOUT===");
    println!("ekernel:        {:#x}", ekernel as *const () as usize);
    println!("MEMORY_END:     {:#x}", MEMORY_END);
    println!("UART_ADDR:      {:#x}", UART_ADDR);
    println!("POWER_OFF_ADDR: {:#x}", POWER_OFF_ADDR);
    println!("MMIO_END:       {:#x}", 0x9000_0000_1fe2_0000 as usize);
}

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
    0xFFFF_FFFF_F000_0000 // 我们计划把sigreturn_trampoline映射到用户页表的这一出
}
