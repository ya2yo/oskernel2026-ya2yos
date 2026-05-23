//! 套接字相关数据结构
use alloc::{boxed::Box, vec::Vec};
use core::{
    any::Any,
    fmt::{self, Debug},
    net::SocketAddr,
    task::Context,
};

use crate::{
    fs::File,
    mm::UserBuffer,
    utils::{SysErrNo, SysResult},
};
// use axio::prelude::*;
use crate::syscall::PollEvents;
use bitflags::bitflags;
use enum_dispatch::enum_dispatch;

use crate::net::{
    options::{Configurable, GetSocketOption, SetSocketOption},
    tcp::TcpSocket,
    udp::UdpSocket,
    unix::{UnixSocket, UnixSocketAddr},
};

/// 套接字地址的扩展，目前只支持ip类型
#[derive(Clone, Debug)]
pub enum SocketAddrEx {
    /// An IP (v4/v6) socket address.
    Ip(SocketAddr),
    /// A Unix domain socket address.
    Unix(UnixSocketAddr),
}

impl SocketAddrEx {
    /// 转换成套接字地址
    pub fn into_ip(self) -> SysResult<SocketAddr> {
        match self {
            SocketAddrEx::Ip(addr) => Ok(addr),
            SocketAddrEx::Unix(_) => Err(SysErrNo::EAFNOSUPPORT),
        }
    }

    pub fn into_unix(self) -> SysResult<UnixSocketAddr> {
        match self {
            SocketAddrEx::Unix(addr) => Ok(addr),
            SocketAddrEx::Ip(_) => Err(SysErrNo::EAFNOSUPPORT),
        }
    }
}

bitflags! {
    /// 向套接字发送数据的标志
    ///
    /// See [`SocketOps::send`].
    #[derive(Default)]
    pub struct SendFlags: u32 {
        const MSG_OOB          = 0x1;
        const MSG_DONTROUTE    = 0x4;
        const MSG_DONTWAIT     = 0x40;
        const MSG_EOR          = 0x80;
        const MSG_CONFIRM      = 0x800;
        const MSG_NOSIGNAL     = 0x4000;
        const MSG_MORE         = 0x8000;
    }
}

bitflags! {
    /// 代表接受数据的标志
    ///
    /// See [`SocketOps::recv`].
    #[derive(Default)]
    pub struct RecvFlags: u32 {
        /// Receive data without removing it from the queue.
        const PEEK = 0x01;
        /// For datagram-like sockets, requires [`SocketOps::recv`] to return
        /// the real size of the datagram, even when it is larger than the
        /// buffer.
        const TRUNCATE = 0x02;
    }
}

/// Type alias for ancillary control message data.
pub type CMsgData = Box<dyn Any + Send + Sync>;

/// 向套接字发送数据的选项
///
/// See [`SocketOps::send`].
#[derive(Default, Debug)]
pub struct SendOptions {
    /// Destination address for the message.
    pub to: Option<SocketAddrEx>,
    /// Send flags.
    pub flags: SendFlags,
    /// Ancillary control messages.
    pub cmsg: Vec<CMsgData>,
}

/// 从套接字接收数据的选项
///
/// See [`SocketOps::recv`].
#[derive(Default)]
pub struct RecvOptions<'a> {
    /// If set, the sender's address is written here.
    pub from: Option<&'a mut SocketAddrEx>,
    /// Receive flags.
    pub flags: RecvFlags,
    /// If set, ancillary control messages are appended here.
    pub cmsg: Option<&'a mut Vec<CMsgData>>,
}
impl Debug for RecvOptions<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvOptions")
            .field("from", &self.from)
            .field("flags", &self.flags)
            .finish()
    }
}

/// 关闭套接字操作的类型
#[derive(Debug, Clone, Copy)]
pub enum Shutdown {
    /// Shut down the read half.
    Read,
    /// Shut down the write half.
    Write,
    /// Shut down both halves.
    Both,
}
impl Shutdown {
    /// Returns `true` if the read half should be shut down.
    pub fn has_read(&self) -> bool {
        matches!(self, Shutdown::Read | Shutdown::Both)
    }

    /// Returns `true` if the write half should be shut down.
    pub fn has_write(&self) -> bool {
        matches!(self, Shutdown::Write | Shutdown::Both)
    }
}

/// 套接字应该实现的方法
#[enum_dispatch]
pub trait SocketOps: Configurable {
    /// Binds an unbound socket to the given address and port.
    fn bind(&self, local_addr: SocketAddrEx) -> SysResult;
    /// Connects the socket to a remote address.
    fn connect(&self, remote_addr: SocketAddrEx) -> SysResult;

    /// Starts listening on the bound address and port.
    fn listen(&self) -> SysResult {
        Err(SysErrNo::EOPNOTSUPP)
    }
    /// Accepts a connection on a listening socket, returning a new socket.
    fn accept(&self) -> SysResult<Socket> {
        Err(SysErrNo::EOPNOTSUPP)
    }

    /// Send data to the socket, optionally to a specific address.
    fn send(&self, src: UserBuffer, options: SendOptions) -> SysResult<usize>;
    /// Receive data from the socket.
    fn recv(&self, dst: UserBuffer, options: RecvOptions<'_>) -> SysResult<usize>;

    /// Get the local endpoint of the socket.
    fn local_addr(&self) -> SysResult<SocketAddrEx>;
    /// Get the remote endpoint of the socket.
    fn peer_addr(&self) -> SysResult<SocketAddrEx>;

    /// Shutdown the socket, closing the connection.
    fn shutdown(&self, how: Shutdown) -> SysResult;
}

/// 网络套接字抽象
#[enum_dispatch(Configurable, SocketOps)]
pub enum Socket {
    /// UDP socket.
    Udp(UdpSocket),
    /// TCP socket.
    Tcp(TcpSocket),
    /// Unix domain socket.
    Unix(UnixSocket),
}

impl File for Socket {
    fn poll(&self, _events: PollEvents) -> PollEvents {
        match self {
            Socket::Tcp(tcp) => tcp.poll(PollEvents::empty()),
            Socket::Udp(udp) => udp.poll(PollEvents::empty()),
            Socket::Unix(unix) => unix.poll(PollEvents::empty()),
        }
    }

    fn register(&self, context: &mut Context<'_>, events: PollEvents) {
        match self {
            Socket::Tcp(tcp) => tcp.register(context, events),
            Socket::Udp(udp) => udp.register(context, events),
            Socket::Unix(unix) => unix.register(context, events),
        }
    }
}
