//! Loongson 2K1000 physical layout and MMIO definitions.

use crate::config::THREAD_MAX_NUM;

pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = 12;
pub const USER_STACK_SIZE: usize = 1024 * 1024 * 8;
pub const KERNEL_STACK_SIZE: usize = PAGE_SIZE * 2;
pub const KERNEL_HEAP_SIZE: usize = 0x8_000_000;
pub const USER_HEAP_SIZE: usize = 0x8000_0000;
pub const MAX_BRK_SIZE: usize = 0x8000_0000;
pub const MAX_MMAP_SIZE: usize = 0x1_0000_0000;
pub const PRE_ALLOC_PAGES: usize = 8;

pub const KERNEL_ADDR_OFFSET: usize = 0x9000_0000_0000_0000;
pub const KERNEL_PGNUM_OFFSET: usize = KERNEL_ADDR_OFFSET >> PAGE_SIZE_BITS;
pub const USER_SPACE_SIZE: usize = 0x30_0000_0000;
pub const USER_TRAP_CONTEXT_TOP: usize = USER_SPACE_SIZE;
pub const USER_STACK_TOP: usize = USER_TRAP_CONTEXT_TOP - PAGE_SIZE * THREAD_MAX_NUM;
pub const MMAP_TOP: usize = USER_TRAP_CONTEXT_TOP
    - PAGE_SIZE * THREAD_MAX_NUM
    - USER_STACK_SIZE * THREAD_MAX_NUM
    - PAGE_SIZE;
pub const DL_INTERP_OFFSET: usize = 0x15_0000_0000;
pub const KSTACK_TOP: usize = usize::MAX - PAGE_SIZE + 1;

/// Linux `loongson-2k1000.dtsi`: UART0 is an 8-bit ns16550a at this address.
pub const UART_ADDR: usize = 0x1fe2_0000;
/// Linux `syscon-poweroff`: PMC + 0x14, writing this value requests power-off.
pub const POWER_OFF_ADDR: usize = 0x1fe2_7014;
pub const POWER_OFF_VALUE: u32 = 0x3c00;

pub const MMIO_MAP_OFFSET: usize = 0xffff_ffff_0000_0000;
pub const VIRTIO_PCI_MMIO_BASE: usize = 0x4000_0000;
pub const VIRTIO_PCI_MMIO_SIZE: usize = 0x10_0000;
pub const MMIO: &[(usize, usize)] = &[(0x1fe0_0000, 0x0003_0000), (0x4000_0000, 0x0100_0000)];

pub fn print_memlayout() {
    extern "C" {
        fn ekernel();
    }
    println!("===MEMLAYOUT(2K1000)===");
    println!("ekernel:        {:#x}", ekernel as *const () as usize);
    for index in 0..crate::arch::hardware::ram_range_count() {
        if let Some((start, size)) = crate::arch::hardware::ram_range(index) {
            println!("RAM[{}]:        [{:#x}, {:#x})", index, start, start + size);
        }
    }
    println!("UART_ADDR:      {:#x}", UART_ADDR);
    println!("POWER_OFF_ADDR: {:#x}", POWER_OFF_ADDR);
}

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
    0xffff_ffff_f000_0000
}
