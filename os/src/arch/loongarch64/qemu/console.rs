//! Uart 16550.

use core::fmt::Write;
// use spinlock::SpinNoIrq;
use super::memory_layout::{KERNEL_ADDR_OFFSET, UART_ADDR};
use spin::Mutex;

// TODO: 也许应该想办法使用页表来避免对这些的访问进入cache...
static COM1: Mutex<Uart16550> = Mutex::new(Uart16550::new(UART_ADDR + KERNEL_ADDR_OFFSET));

// 实现console的putchar和getchar
struct Uart16550 {
    base_address: usize,
}

impl Uart16550 {
    pub const fn new(base_address: usize) -> Self {
        Uart16550 { base_address }
    }

    pub fn putchar(&mut self, c: u8) {
        let ptr = self.base_address as *mut u8;
        loop {
            unsafe {
                let c = ptr.add(5).read_volatile();
                if c & (1 << 5) != 0 {
                    break;
                }
            }
        }
        unsafe {
            ptr.add(0).write_volatile(c);
        }
    }

    pub fn getchar(&mut self) -> Option<u8> {
        let ptr = self.base_address as *mut u8;
        unsafe {
            if ptr.add(5).read_volatile() & 1 == 0 {
                // The DR bit is 0, meaning no data
                None
            } else {
                // The DR bit is 1, meaning data!
                Some(ptr.add(0).read_volatile())
            }
        }
    }
}

/// Writes a byte to the console.
pub fn console_putchar(c: u8) {
    let mut uart = COM1.lock();
    match c {
        b'\n' => {
            uart.putchar(b'\r');
            uart.putchar(b'\n');
        }
        c => uart.putchar(c),
    }
}

/// Reads a byte from the console, or returns [`None`] if no input is available.
pub fn console_getchar() -> Option<u8> {
    COM1.lock().getchar()
}
