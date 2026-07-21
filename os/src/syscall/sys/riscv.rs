//! RISC-V-specific syscall entry points.

use crate::utils::{SysErrNo, SyscallRet};

/// Stub for `riscv_hwprobe(2)`.
///
/// Returning success without populating the caller's probe pairs would make
/// userspace consume invalid hardware capability data.
pub fn sys_riscv_hwprobe(_pairs: *mut u8, _pair_count: usize, _cpu_set_size: usize) -> SyscallRet {
    Err(SysErrNo::ENOSYS)
}
