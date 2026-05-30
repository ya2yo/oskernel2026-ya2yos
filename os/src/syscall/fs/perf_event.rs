use linux_raw_sys::ctypes::{c_int, c_ulong};
use log::warn;

use crate::{syscall::fs::dummyfd_create, utils::SyscallRet};

/// https://man7.org/linux/man-pages/man2/perf_event_open.2.html
pub fn sys_perf_event_open(attr: *mut u8, pid: u32, cpu: c_int, group_fd: c_int, flags: u32) -> SyscallRet {
    warn!("[sys_perf_event_open] not implement!");
    dummyfd_create()
}