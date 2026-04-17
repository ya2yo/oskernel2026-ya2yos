use core::arch::asm;
use sbi_rt::{system_reset, NoReason, Shutdown, SystemFailure};

pub fn hart_id() -> usize {
    // let hartid;
    // unsafe {
    //     asm! {
    //         "mv {}, tp",
    //         out(reg) hartid
    //     };
    // }
    // hartid

    // TODO: 多核时应当将其改掉
    0
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
