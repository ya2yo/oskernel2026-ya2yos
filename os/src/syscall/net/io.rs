use log::{debug, warn};

use crate::utils::SyscallRet;

/// 参考 https://man7.org/linux/man-pages/man2/sendto.2.html
pub fn sys_sendto(
    _sockfd: usize,
    _buf: *const u8,
    _len: usize,
    _flags: u32,
    _dest_addr: *const u8,
    _addrlen: u32,
) -> SyscallRet {
    debug!("[sys_sendto] fd={}", _sockfd,);
    todo!("sys_sendto")
}

/// 参考 https://man7.org/linux/man-pages/man2/recvfrom.2.html
pub fn sys_recvfrom(
    _sockfd: usize,
    _buf: *mut u8,
    _len: usize,
    _flags: u32,
    _src_addr: *const u8,
    _addrlen: u32,
) -> SyscallRet {
    debug!("ENTER recvfrom");
    todo!("recvfrom")
}

/// 参考 https://man7.org/linux/man-pages/man2/sendmsg.2.html
pub fn sys_sendmsg(_sockfd: usize, _addr: *const u8, _flags: u32) -> SyscallRet {
    warn!("[sys_sendmsg] fd={}", _sockfd,);
    warn!("sys_sendmsg is not implemented, return Ok(0)");
    Ok(0)
}