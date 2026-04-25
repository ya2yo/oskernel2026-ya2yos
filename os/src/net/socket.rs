use alloc::{boxed::Box, vec::Vec};
use core::{
    any::Any,
    fmt::{self, Debug},
    net::SocketAddr,
    task::Context,
};


use crate::{fs::File, utils::{SysErrNo,SysResult}};
// use axio::prelude::*;
use crate::syscall::PollEvents;
use bitflags::bitflags;
use enum_dispatch::enum_dispatch;

use crate::net::{
    options::{Configurable, GetSocketOption, SetSocketOption},
    tcp::TcpSocket,
    udp::UdpSocket,
};

/// Extended socket address supporting IP families.
#[derive(Clone, Debug)]
pub enum SocketAddrEx {
    /// An IP (v4/v6) socket address.
    Ip(SocketAddr),
}

impl SocketAddrEx {
    /// Convert into an IP socket address, or return an error if not IP.
    pub fn into_ip(self) -> SysErrResult<SocketAddr> {
        match self {
            SocketAddrEx::Ip(addr) => Ok(addr),
        }
    }
}

bitflags! {
    /// Flags for sending data to a socket.
    ///
    /// See [`SocketOps::send`].
    #[derive(Default, Debug, Clone, Copy)]
    pub struct SendFlags: u32 {
    }
}

bitflags! {
    /// Flags for receiving data from a socket.
    ///
    /// See [`SocketOps::recv`].
    #[derive(Default, Debug, Clone, Copy)]
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

/// Options for sending data to a socket.
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

/// Options for receiving data from a socket.
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

/// Kind of shutdown operation to perform on a socket.
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

/// Operations that can be performed on a socket.
#[enum_dispatch]
pub trait SocketOps: Configurable {
    /// Binds an unbound socket to the given address and port.
    fn bind(&self, local_addr: SocketAddrEx) -> SysErrResult;
    /// Connects the socket to a remote address.
    fn connect(&self, remote_addr: SocketAddrEx) -> SysErrResult;

    /// Starts listening on the bound address and port.
    fn listen(&self) -> SysErrResult {
        Err(SysErrNo::OperationNotSupported)
    }
    /// Accepts a connection on a listening socket, returning a new socket.
    fn accept(&self) -> SysErrResult<Socket> {
        Err(SysErrNo::OperationNotSupported)
    }

    /// Send data to the socket, optionally to a specific address.
    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> SysErrResult<usize>;
    /// Receive data from the socket.
    fn recv(&self, dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> SysErrResult<usize>;

    /// Get the local endpoint of the socket.
    fn local_addr(&self) -> SysErrResult<SocketAddrEx>;
    /// Get the remote endpoint of the socket.
    fn peer_addr(&self) -> SysErrResult<SocketAddrEx>;

    /// Shutdown the socket, closing the connection.
    fn shutdown(&self, how: Shutdown) -> SysErrResult;
}

/// Network socket abstraction.
#[enum_dispatch(Configurable, SocketOps)]
pub enum Socket {
    /// UDP socket.
    Udp(UdpSocket),
    /// TCP socket.
    Tcp(TcpSocket),
}

impl File for Socket {
    fn poll(&self) -> PollEvents {
        match self {
            Socket::Tcp(tcp) => tcp.poll(PollEvents::empty()),
            Socket::Udp(udp) => udp.poll(),
        }
    }

    fn register(&self, context: &mut Context<'_>, events: PollEvents) {
        match self {
            Socket::Tcp(tcp) => tcp.register(context, events),
            Socket::Udp(udp) => udp.register(context, events),
        }
    }
}
