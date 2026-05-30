use core::ffi::c_char;

use linux_raw_sys::ctypes::c_int;
use log::warn;

use crate::utils::SyscallRet;

/// https://man7.org/linux/man-pages/man2/inotify_init1.2.html
pub fn sys_inotify_init1(_flags: u32) -> SyscallRet {
    warn!("[sys_inotify_init1] not implement!");
    Ok(0)
}
/// https://man7.org/linux/man-pages/man2/inotify_add_watch.2.html
pub fn sys_inotify_add_watch(_fd: c_int, _path: *const c_char, _mask: u32)-> SyscallRet {
    warn!("[sys_inotify_add_watch] not implement!");
    Ok(0)
}
pub fn sys_inotify_rm_watch(_fd: c_int, _wd: c_int) -> SyscallRet {
    warn!("[sys_inotify_rm_watch] not implement!");
    Ok(0)
}