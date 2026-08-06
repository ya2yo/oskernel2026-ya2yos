use core::arch::asm;
use riscv::register::sie;
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

/// Wait until this hart receives a timer or a targeted scheduler wakeup while
/// its run queue is empty.
///
/// `run_tasks` normally runs with supervisor interrupts disabled.  RISC-V WFI
/// still resumes when a locally enabled interrupt becomes pending in that
/// state, so no trap is taken through the non-returning kernel trap entry. A
/// scheduler IPI is enabled only while idle and cleared before user mode can
/// observe it, keeping this path independent of user trap handling.
pub fn idle() {
    unsafe {
        asm!("csrci sstatus, 2", options(nostack));
        sie::set_ssoft();
        crate::timer::set_next_trigger();
        riscv::asm::wfi();
        sie::clear_ssoft();
        clear_ipi();
    }
    crate::mm::remote_tlb::poll();
    crate::timer::set_next_trigger();
}

/// Acknowledge the SBI supervisor software interrupt source.
#[inline]
pub fn clear_ipi() {
    #[allow(deprecated)]
    {
        sbi_rt::legacy::clear_ipi();
    }
}

/// Wake a hart that has published an idle state after receiving a runnable
/// task.  The SBI sPI extension delivers a supervisor software interrupt.
pub fn wake_hart(hartid: usize) -> bool {
    if hartid >= usize::BITS as usize {
        return false;
    }
    sbi_rt::send_ipi(1usize << hartid, 0).is_ok()
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
