//! `rseq(2)` syscall entry point.

use crate::{task::current_task, utils::SyscallRet};

/// Register or unregister the current thread's classic 32-byte rseq ABI area.
/// Core state and user-return fixup semantics are kept in `task::rseq`.
pub fn sys_rseq(rseq: *mut u8, rseq_len: u32, flags: u32, sig: u32) -> SyscallRet {
    current_task()
        .unwrap()
        .rseq(rseq as usize, rseq_len, flags, sig)?;
    Ok(0)
}
