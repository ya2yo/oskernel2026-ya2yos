use log::warn;

use crate::utils::SyscallRet;

/// https://man7.org/linux/man-pages/man2/pidfd_open.2.html
pub fn sys_pidfd_open(pid: u32, flags: u32)->SyscallRet {
    warn!("[sys_pidfd_open] not implement!");
    Ok(0)
}
/// https://man7.org/linux/man-pages/man2/pidfd_getfd.2.html
pub fn sys_pidfd_getfd(pidfd: i32, target_fd: i32, flags: u32)->SyscallRet {
    warn!("[sys_pidfd_getfd] not implement!");
    Ok(0)
}