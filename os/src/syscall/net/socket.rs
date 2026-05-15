use super::consts::*;
use crate::fs::{FdTable, File, FileClass, FileDescriptor, OpenFlags, Socket};
use crate::net::tcp::TcpSocket;
use crate::net::udp::UdpSocket;
use crate::net::{Socket as SocketInner, SocketAddrEx};
use crate::syscall::net::addr::SocketAddrExt;
use crate::{
    task::{current_task, Process},
    utils::{SysErrNo, SyscallRet},
};
use crate::net::SocketOps;
use alloc::sync::Arc;
use log::{debug, warn};

/// 参考 https://man7.org/linux/man-pages/man2/socket.2.html
pub fn sys_socket(domain: u32, raw_ty: u32, proto: u32) -> SyscallRet {
    debug!("sys_socket <= domain: {domain}, ty: {raw_ty}, proto: {proto}");
    // 提取 socket类型
    let ty = raw_ty & 0xFF;
    let task = current_task().unwrap();
    let socket_inner = match (domain, ty) {
        (AF_INET, SOCK_STREAM) => {
            if proto != 0 && proto != IPPROTO_TCP as _ {
                return Err(SysErrNo::EPROTONOSUPPORT);
            }
            SocketInner::Tcp(TcpSocket::new())
        }
        (AF_INET, SOCK_DGRAM) => {
            if proto != 0 && proto != IPPROTO_UDP as _ {
                return Err(SysErrNo::EPROTONOSUPPORT);
            }
            SocketInner::Udp(UdpSocket::new())
        }
        // (AF_UNIX, SOCK_STREAM) => SocketInner::Unix(UnixSocket::new(StreamTransport::new(pid))),
        // (AF_UNIX, SOCK_DGRAM) => SocketInner::Unix(UnixSocket::new(DgramTransport::new(pid))),
        (AF_INET, _) | (AF_UNIX, _) | (AF_VSOCK, _) => {
            warn!("Unsupported socket type: domain: {domain}, ty: {ty}");
            return Err(SysErrNo::ESOCKTNOSUPPORT);
        }
        _ => {
            return Err(SysErrNo::EAFNOSUPPORT);
        }
    };
    let socket = Arc::new(Socket(socket_inner));

    let proc_inner = task.process.inner_lock();
    let fd_table = proc_inner.fd_table.clone();
    let new_fd = fd_table.alloc_fd()?;
    let mut open_flags = OpenFlags::empty();

    if raw_ty & OpenFlags::O_CLOEXEC.bits() != 0 {
        open_flags |= OpenFlags::O_CLOEXEC;
    }

    let file_desc = FileDescriptor::new(open_flags, FileClass::Socket(socket));

    fd_table.set(new_fd, file_desc)?;
    Ok(new_fd)
}

/// 参考 https://www.man7.org/linux/man-pages/man2/socketpair.2.html
pub fn sys_socketpair(domain: u32, stype: u32, protocol: u32, sv: *mut u32) -> SyscallRet {
    debug!(
        "[sys_socketpair] domain is {}, type is {}, protocol is {}, sv is {}",
        domain, stype, protocol, sv as usize
    );
    todo!("socketpair")
}

/// 参考 https://man7.org/linux/man-pages/man2/bind.2.html
pub fn sys_bind(sockfd: usize, addr: *const u8, addrlen: u32) -> SyscallRet {
    debug!(
        "[sys_bind] fd={}, addr={}, len={}",
        sockfd, addr as usize, addrlen
    );
    let addr = SocketAddrEx::read_from_user(addr, addrlen)?;
    Socket::from_fd(sockfd)?.0.bind(addr)?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/listen.2.html
pub fn sys_listen(_sockfd: usize, _backlog: u32) -> SyscallRet {
    warn!("[sys_listen] fd={}", _sockfd,);
    warn!("sys_listen is not implemented, return Ok(0)");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/accept.2.html
pub fn sys_accept(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    warn!("[sys_accept] fd={}", _sockfd,);
    warn!("sys_accept is not implemented, return Ok(0)");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/connect.2.html
pub fn sys_connect(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    warn!("[sys_connect] fd={}", _sockfd,);
    warn!("sys_connect is not implemented, return Ok(0)");
    Ok(0)
}

pub fn sys_accept4(_sockfd: usize, _addr: *const u8, _addrlen: u32, _flags: u32) -> SyscallRet {
    warn!("[sys_accept4] fd={}", _sockfd,);
    warn!("sys_accept4 is not implemented, return Ok(0)");
    Ok(0)
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn test_socket_logic(){
    }

}
