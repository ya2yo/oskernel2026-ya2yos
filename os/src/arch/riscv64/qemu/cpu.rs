use core::arch::asm;
use sbi_rt::{system_reset, NoReason, Shutdown, SystemFailure};

use crate::arch::{config::HART_NUM, memory_layout::KERNEL_ADDR_OFFSET};

pub fn hart_id() -> usize {
    let hartid: usize;
    unsafe {
        asm!("mv {}, tp", out(reg) hartid, options(nomem, nostack, preserves_flags));
    }
    hartid
}

/// Start secondary harts once the bootstrap hart has completed global setup.
/// SBI expects a physical supervisor entry address, while this kernel is linked
/// through its high-half direct map.
pub fn boot_secondary_harts(boot_hart: usize) {
    extern "C" {
        fn _start();
    }

    let start_addr = _start as *const () as usize - KERNEL_ADDR_OFFSET;
    for hart in 0..HART_NUM {
        if hart == boot_hart {
            continue;
        }
        let ret = sbi_rt::hart_start(hart, start_addr, 0);
        if !ret.is_ok() {
            panic!(
                "SBI hart_start failed for hart {}: error={}, value={}",
                hart, ret.error, ret.value
            );
        }
    }
}

/// use sbi call to shutdown the kernel
pub fn shutdown(failure: bool) -> ! {
    if !failure {
        system_reset(Shutdown, NoReason);
    } else {
        system_reset(Shutdown, SystemFailure);
    }
    unreachable!()
}
