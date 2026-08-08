//! RISC-V-specific syscall entry points.

use crate::utils::{SysErrNo, SyscallRet};

const SYS_RISCV_FLUSH_ICACHE_LOCAL: usize = 1;

/// Stub for `riscv_hwprobe(2)`.
///
/// Returning success without populating the caller's probe pairs would make
/// userspace consume invalid hardware capability data.
pub fn sys_riscv_hwprobe(_pairs: *mut u8, _pair_count: usize, _cpu_set_size: usize) -> SyscallRet {
    Err(SysErrNo::ENOSYS)
}

/// `riscv_flush_icache(2)`.
///
/// The architecture backend currently exposes a hart-local instruction fence.
/// The address range is accepted for ABI compatibility but does not change the
/// fence granularity.
pub fn sys_riscv_flush_icache(_start: usize, _end: usize, flags: usize) -> SyscallRet {
    if flags & !SYS_RISCV_FLUSH_ICACHE_LOCAL != 0 {
        return Err(SysErrNo::EINVAL);
    }
    crate::arch::tlb::instruction_fence();
    Ok(0)
}
