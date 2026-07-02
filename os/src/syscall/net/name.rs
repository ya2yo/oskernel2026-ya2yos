use log::debug;

use crate::fs::Socket;
use crate::mm::copy_from_user;
use crate::net::SocketOps;
use crate::syscall::net::addr::SocketAddrExt;
use crate::task::current_task;
use crate::utils::{SysErrNo, SysResult, SyscallRet};

/// 从用户空间读取 socklen_t 值
/// - `addrlen_ptr`: 用户空间中 socklen_t 变量的指针
///
/// 成功返回读取到的长度值，失败返回 EFAULT（指针无效）或 EINVAL（值为负数）
fn read_addrlen_from_user(addrlen_ptr: usize) -> SysResult<u32> {
    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.memory_set_arc();
    let mut buf = [0u8; 4];
    copy_from_user(&memory_set, addrlen_ptr, &mut buf).map(|_| ())?;
    // 以 i32 读取以检测负数（Linux 中 socklen_t 为 unsigned int，
    // 但内核将其作为 int 读取以检测 EINVAL）
    let val = i32::from_ne_bytes(buf);
    if val < 0 {
        return Err(SysErrNo::EINVAL);
    }
    Ok(val as u32)
}

/// getsockname 和 getpeername 的公共逻辑
/// - `sockfd`: 套接字文件描述符
/// - `addr`: 用户空间的目标缓冲区指针
/// - `addrlen`: 已验证的用户提供的缓冲区大小（值，非指针）
/// - `get_addr`: 获取地址的闭包，对应 `local_addr` 或 `peer_addr`
fn get_socket_name(
    sockfd: usize,
    addr: *mut u8,
    mut addrlen: u32,
    get_addr: impl Fn(&crate::net::Socket) -> crate::utils::SysResult<crate::net::SocketAddrEx>,
) -> SyscallRet {
    let socket = Socket::from_fd(sockfd)?;
    let sock_addr = get_addr(&socket)?;
    // debug!("get_socket_name: sockfd={sockfd}, addr={sock_addr:?}");

    if !addr.is_null() && addrlen > 0 {
        sock_addr.write_to_user(addr, &mut addrlen)?;
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/getsockname.2.html
pub fn sys_getsockname(sockfd: usize, addr: *const u8, addrlen_ptr: usize) -> SyscallRet {
    let addrlen = read_addrlen_from_user(addrlen_ptr)?;
    get_socket_name(sockfd, addr as *mut u8, addrlen, |socket| {
        socket.local_addr()
    })
}

/// 参考 https://man7.org/linux/man-pages/man2/getpeername.2.html
pub fn sys_getpeername(sockfd: usize, addr: *const u8, addrlen_ptr: usize) -> SyscallRet {
    let addrlen = read_addrlen_from_user(addrlen_ptr)?;
    get_socket_name(sockfd, addr as *mut u8, addrlen, |socket| {
        socket.peer_addr()
    })
}
