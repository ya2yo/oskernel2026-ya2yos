use log::warn;

use crate::utils::SyscallRet;

/// https://www.man7.org/linux/man-pages/man2/timerfd_create.2.html
pub fn sys_timerfd_create(clockid: u32, flags: u32)->SyscallRet{
    warn!("[sys_timerfd_create] not implement!");
    Ok(0)
}
pub fn sys_timerfd_settime(fd: u32, flags: u32, new_value: *const u8,old_value: *mut u8)->SyscallRet {
    warn!("[sys_timerfd_settime] not implement!");
    Ok(0)
}
pub fn sys_timerfd_gettime(fd: u32, curr_value: *mut u8)->SyscallRet {
    warn!("[sys_timerfd_gettime] not implement!");
    Ok(0)
}