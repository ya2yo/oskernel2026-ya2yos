use crate::utils::{SysResult, SyscallRet};

/// 参考 https://man7.org/linux/man-pages/man2/umask.2.html
pub fn sys_umask(_mask: u32) -> SyscallRet {
    Ok(0)
}

// https://man7.org/linux/man-pages/man2/get_mempolicy.2.html
pub fn sys_get_mempolicy(
    _policy: usize,
    _nodemask: usize,
    _maxnode: usize,
    _addr: usize,
    _flags: usize,
) -> SysResult<usize> {
    log::error!("Unimplemented sys_get_mempolicy");
    Ok(0)
}
