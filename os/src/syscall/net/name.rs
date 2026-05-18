use log::debug;

use crate::fs::Socket;
use crate::net::SocketOps;
use crate::syscall::net::addr::SocketAddrExt;
use crate::utils::SyscallRet;

/// getsockname 和 getpeername 的公共逻辑
/// - `sockfd`: 套接字文件描述符
/// - `addr`: 用户空间的目标缓冲区指针
/// - `addrlen`: 用户提供的缓冲区大小
/// - `get_addr`: 获取地址的闭包，对应 `local_addr` 或 `peer_addr`
fn get_socket_name(
    sockfd: usize,
    addr: *mut u8,
    mut addrlen: u32,
    get_addr: impl Fn(&crate::net::Socket) -> crate::utils::SysResult<crate::net::SocketAddrEx>,
) -> SyscallRet {
    let socket = Socket::from_fd(sockfd)?;
    let sock_addr = get_addr(&socket)?;
    debug!("get_socket_name: sockfd={sockfd}, addr={sock_addr:?}");

    if !addr.is_null() && addrlen > 0 {
        sock_addr.write_to_user(addr, &mut addrlen)?;
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/getsockname.2.html
pub fn sys_getsockname(sockfd: usize, addr: *const u8, addrlen: usize) -> SyscallRet {
    get_socket_name(sockfd, addr as *mut u8, addrlen as u32, |socket| {
        socket.local_addr()
    })
}

/// 参考 https://man7.org/linux/man-pages/man2/getpeername.2.html
pub fn sys_getpeername(sockfd: usize, addr: *const u8, addrlen: u32) -> SyscallRet {
    get_socket_name(sockfd, addr as *mut u8, addrlen, |socket| {
        socket.peer_addr()
    })
}
