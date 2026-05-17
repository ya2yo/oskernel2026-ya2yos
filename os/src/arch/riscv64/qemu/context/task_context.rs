//! Implementation of [`TaskContext`]
use crate::trap::trap_return;

#[repr(C)]
/// task context structure containing some registers
pub struct TaskContext {
    /// return address ( e.g. __return_to_user ) of __switch ASM function
    pub ra: usize,
    /// kernel stack pointer of app
    pub sp: usize,
    /// s0-11 register, callee saved
    s: [usize; 12],
}

impl TaskContext {
    /// init task context
    pub fn zero_init() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }

    // 现在我们要去往trap_return了，而不是trap_loop
    pub fn goto_trap_return(kstack_ptr: usize) -> Self {
        Self {
            ra: trap_return as *const () as usize,
            sp: kstack_ptr,
            s: [0; 12],
        }
    }
}
