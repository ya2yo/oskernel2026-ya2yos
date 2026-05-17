use crate::{task::current_task, utils::SyscallRet};

/// 参考 https://man7.org/linux/man-pages/man2/getpid.2.html
pub fn sys_getpid() -> SyscallRet {
    Ok(current_task().unwrap().pid())
}

/// 参考 https://man7.org/linux/man-pages/man2/getppid.2.html
pub fn sys_getppid() -> SyscallRet {
    Ok(current_task().unwrap().ppid())
}

/// 参考 https://man7.org/linux/man-pages/man2/gettid.2.html
pub fn sys_gettid() -> SyscallRet {
    Ok(current_task().unwrap().tid())
}

/// 参考 https://man7.org/linux/man-pages/man2/set_tid_address.2.html
pub fn sys_settidaddress(tidptr: usize) -> SyscallRet {
    current_task().unwrap().inner_lock().clear_child_tid = tidptr;
    sys_gettid()
}
