//! JH7110 UART0, compatible with the standard 16550 register layout.

use core::ptr::{read_volatile, write_volatile};

use super::memory_layout::KERNEL_ADDR_OFFSET;

const UART0_PA: usize = 0x1000_0000;
const UART0: usize = UART0_PA + KERNEL_ADDR_OFFSET;
const RBR_THR: usize = 0;
const LSR: usize = 5;
const LSR_DR: u8 = 1 << 0;
const LSR_THRE: u8 = 1 << 5;

#[inline]
fn reg(offset: usize) -> *mut u8 {
    (UART0 + offset) as *mut u8
}

pub fn putchar(c: u8) {
    while unsafe { read_volatile(reg(LSR)) } & LSR_THRE == 0 {
        core::hint::spin_loop();
    }
    unsafe { write_volatile(reg(RBR_THR), c) };
}

pub fn getchar() -> Option<u8> {
    if unsafe { read_volatile(reg(LSR)) } & LSR_DR == 0 {
        None
    } else {
        Some(unsafe { read_volatile(reg(RBR_THR)) })
    }
}
