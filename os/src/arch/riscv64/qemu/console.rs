//! SBI call wrappers
#![allow(unused)]

use sbi_rt::SbiRet;

/// use sbi call to putchar in console (qemu uart handler)
pub fn console_putchar(c: u8) {
    #[allow(deprecated)]
    sbi_rt::legacy::console_putchar(c as usize);
}

/// use sbi call to getchar from console (qemu uart handler)
pub fn console_getchar() -> Option<u8> {
    #[allow(deprecated)]
    let c = sbi_rt::legacy::console_getchar();
    if c > 255 {
        None
    } else {
        Some(c as u8)
    }
}
