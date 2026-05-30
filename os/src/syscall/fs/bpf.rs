use log::warn;

use crate::{syscall::fs::dummyfd_create, utils::SyscallRet};

/// https://man7.org/linux/man-pages/man2/bpf.2.html
pub fn sys_bpf(_cmd: i32, _attr: *mut u8, _size: u32) -> SyscallRet {
    warn!("[sys_bpf] not implement!");
    dummyfd_create()
}