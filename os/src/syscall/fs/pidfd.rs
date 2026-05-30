use log::warn;

use crate::{syscall::fs::dummyfd_create, utils::SyscallRet};

/// https://man7.org/linux/man-pages/man2/pidfd_open.2.html
pub fn sys_pidfd_open(_pid: u32, _flags: u32) -> SyscallRet {
    warn!("[sys_pidfd_open] not implement!");
    dummyfd_create()
}
/// https://man7.org/linux/man-pages/man2/pidfd_getfd.2.html
pub fn sys_pidfd_getfd(_pidfd: i32, _target_fd: i32, _flags: u32) -> SyscallRet {
    warn!("[sys_pidfd_getfd] not implement!");
    Ok(0)
}
