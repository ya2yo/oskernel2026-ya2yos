use alloc::sync::Arc;
use log::{debug, warn};
use crate::fs::{FdTable, File, FileClass, FileDescriptor, OpenFlags, Socket};
use crate::net::Socket as SocketInner;
use crate::net::tcp::TcpSocket;
use crate::net::udp::UdpSocket;
use crate::{task::{Process, current_task}, utils::{SysErrNo, SyscallRet}};
use super::consts::*;

/// 参考 https://man7.org/linux/man-pages/man2/socket.2.html
pub fn sys_socket(domain: u32, raw_ty: u32, proto: u32) -> SyscallRet {
    debug!("sys_socket <= domain: {domain}, ty: {raw_ty}, proto: {proto}");
    // 提取 socket类型
    let ty = raw_ty & 0xFF;
    let task=current_task().unwrap();
    let pid=task.pid();
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

    let proc_inner=task.process.inner_lock();
    let fd_table=proc_inner.fd_table.clone();
    let new_fd=fd_table.alloc_fd()?;
    let mut open_flags = OpenFlags::empty();
    
    if raw_ty & OpenFlags::O_CLOEXEC.bits() != 0 {
        open_flags |= OpenFlags::O_CLOEXEC;
    }

    let file_desc = FileDescriptor::new(
        open_flags, 
        FileClass::Socket(socket)
    );

    fd_table.set(new_fd, file_desc)?;
    Ok(new_fd)
}
