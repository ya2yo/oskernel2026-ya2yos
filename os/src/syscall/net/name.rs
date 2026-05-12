use log::warn;

use crate::utils::{SysErrNo, SyscallRet};

/// 参考 https://man7.org/linux/man-pages/man2/getsockname.2.html
pub fn sys_getsockname(fd: usize, addr: *const u8, addr_len: usize) -> SyscallRet {
    warn!(
        "[sys_getsockname] fd={}, addr={}, len={}",
        fd, addr as usize, addr_len
    );
    todo!("sys_getsocketname")
}

/// 参考 https://man7.org/linux/man-pages/man2/getpeername.2.html
pub fn sys_getpeername(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    warn!(
        "[sys_getpeername] fd={}, addr={}, len={}",
        _sockfd, _addr as usize, _addrlen
    );
    warn!("sys_getpeername is not implemented, return Err(SysErrNo::Default)");
    Err(SysErrNo::Default)
}