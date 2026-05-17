use super::consts::*;
use crate::fs::{FdTable, File, FileClass, FileDescriptor, OpenFlags, Socket};
use crate::net::tcp::TcpSocket;
use crate::net::udp::UdpSocket;
use crate::net::SocketOps;
use crate::net::{Shutdown, Socket as SocketInner, SocketAddrEx};
use crate::syscall::net::addr::SocketAddrExt;
use crate::{
    task::{current_task, Process},
    utils::{SysErrNo, SyscallRet},
};
use alloc::sync::Arc;
use linux_raw_sys::general::{O_CLOEXEC, O_NONBLOCK};
use linux_raw_sys::net::{SHUT_RD, SHUT_RDWR, SHUT_WR};
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
pub fn sys_listen(sockfd: usize, backlog: u32) -> SyscallRet {
    debug!("sys_listen <= fd: {}, backlog: {}", sockfd, backlog);
    Socket::from_fd(sockfd)?.listen()?;

    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/accept.2.html
pub fn sys_accept(sockfd: usize, addr: *mut u8, addrlen: u32) -> SyscallRet {
    sys_accept4(sockfd, addr, addrlen, 0)
}

/// 参考 https://man7.org/linux/man-pages/man2/connect.2.html
/// 参考StarryOS kernel/src/syscall/net/socket.rs
pub fn sys_connect(sockfd: usize, addr: *const u8, addrlen: u32) -> SyscallRet {
    let addr = SocketAddrEx::read_from_user(addr, addrlen)?;
    debug!("sys_connect <= fd: {sockfd}, addr: {addr:?}");

    Socket::from_fd(sockfd)?.connect(addr).map_err(|e| {
        if e == SysErrNo::EAGAIN {
            SysErrNo::EINPROGRESS
        } else {
            e
        }
    })?;

    Ok(0)
}
/// https://man7.org/linux/man-pages/man2/shutdown.2.html
pub fn sys_shutdown(sockfd: usize, how: u32) -> SyscallRet {
    debug!("sys_shutdown <= fd: {sockfd}, how: {how:?}");

    let socket = Socket::from_fd(sockfd)?;
    let how = match how {
        SHUT_RD => Shutdown::Read,
        SHUT_WR => Shutdown::Write,
        SHUT_RDWR => Shutdown::Both,
        _ => return Err(SysErrNo::EINVAL),
    };
    socket.shutdown(how).map(|_| 0)
}

pub fn sys_accept4(sockfd: usize, addr: *mut u8, mut addrlen: u32, flags: u32) -> SyscallRet {
    debug!("sys_accept <= fd: {}, flags: {}", sockfd, flags);
    let socket = Socket::from_fd(sockfd)?;
    let socket = Socket(socket.accept()?);
    let remote_addr = socket.local_addr()?;
    if !addr.is_null() {
        remote_addr.write_to_user(addr, &mut addrlen);
    }
    // 分配新的fd
    let fd_table = current_task().unwrap().get_fd_table();
    let fd = fd_table.alloc_fd()?;
    // 将新的fd连接到旧的上面
    let mut new_flags = OpenFlags::empty();
    if flags & OpenFlags::O_NONBLOCK.bits() != 0 {
        new_flags.insert(OpenFlags::O_NONBLOCK);
        socket.set_nonblocking(true);
    }
    if flags & OpenFlags::O_CLOEXEC.bits() != 0 {
        new_flags.insert(OpenFlags::O_CLOEXEC);
    }

    fd_table.set(
        fd,
        FileDescriptor::new(new_flags, FileClass::Socket(Arc::new(socket))),
    );

    Ok(fd)
}
