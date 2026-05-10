//! 该文件实际上实现了socket相关系统调用

use alloc::{collections::vec_deque::VecDeque, format, string::ToString, vec::Vec};
use spin::{Lazy, Mutex};

use crate::{
    fs::{
        FileClass, FileDescriptor, OpenFlags,
    }, mm::{get_data, put_data, safe_translated_byte_buffer, translated_refmut}, net::{Socket, SocketOps}, task::{current_task, current_token}, utils::{SysErrNo, SyscallRet}
};
use log::{debug, warn};

pub static UDP_QUEUE: Lazy<Mutex<VecDeque<Vec<u8>>>> = Lazy::new(|| Mutex::new(VecDeque::new()));

/// 参考 https://man7.org/linux/man-pages/man2/socket.2.html
pub fn sys_socket(_domain: u32, _type: u32, _protocol: u32) -> SyscallRet {
    warn!(
        "[sys_socket] domain={}, type={}, protocol={}",
        _domain, _type, _protocol
    );
    unimplemented!("sys_socket not done")
}

/// 参考 https://man7.org/linux/man-pages/man2/bind.2.html
pub fn sys_bind(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    debug!(
        "[sys_bind] fd={}, addr={}, len={}",
        _sockfd, _addr as usize, _addrlen
    );
    unimplemented!("sys_bind not done")
}

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

/// 参考 https://man7.org/linux/man-pages/man2/setsockopt.2.html
pub fn sys_setsockopt(
    _sockfd: usize,
    _level: u32,
    _optname: u32,
    _optcal: *const u8,
    _optlen: u32,
) -> SyscallRet {
    warn!("[sys_setsockopt] fd={}", _sockfd,);
    warn!("sys_setsockopt is not implemented, return Ok(0)");

    Ok(0)
}

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

/// 参考 https://man7.org/linux/man-pages/man2/listen.2.html
pub fn sys_listen(_sockfd: usize, _backlog: u32) -> SyscallRet {
    warn!("[sys_listen] fd={}", _sockfd,);
    warn!("sys_listen is not implemented, return Ok(0)");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/connect.2.html
pub fn sys_connect(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    warn!("[sys_connect] fd={}", _sockfd,);
    warn!("sys_connect is not implemented, return Ok(0)");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/accept.2.html
pub fn sys_accept(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    warn!("[sys_accept] fd={}", _sockfd,);
    warn!("sys_accept is not implemented, return Ok(0)");
    Ok(0)
}

pub fn sys_accept4(_sockfd: usize, _addr: *const u8, _addrlen: u32, _flags: u32) -> SyscallRet {
    warn!("[sys_accept4] fd={}", _sockfd,);
    warn!("sys_accept4 is not implemented, return Ok(0)");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sendmsg.2.html
pub fn sys_sendmsg(_sockfd: usize, _addr: *const u8, _flags: u32) -> SyscallRet {
    warn!("[sys_sendmsg] fd={}", _sockfd,);
    warn!("sys_sendmsg is not implemented, return Ok(0)");
    Ok(0)
}

pub fn sys_socketpair(domain: u32, stype: u32, protocol: u32, sv: *mut u32) -> SyscallRet {
    debug!(
        "[sys_socketpair] domain is {}, type is {}, protocol is {}, sv is {}",
        domain, stype, protocol, sv as usize
    );
    todo!("socketpair")
}
