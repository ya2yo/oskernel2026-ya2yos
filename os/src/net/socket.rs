//! 套接字相关数据结构
use alloc::{boxed::Box, vec::Vec};
use core::{
    any::Any,
    fmt::{self, Debug},
    net::SocketAddr,
    task::Context,
};
use linux_raw_sys::net::*;

use crate::{
    fs::File,
    mm::UserBuffer,
    utils::{SysErrNo, SysResult},
};
// use axio::prelude::*;
use crate::syscall::PollEvents;
use bitflags::bitflags;

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
        // 在流式套接字上发送带外数据。
        const OOB          = 0x1;
        // 不要使用网关发送数据包，仅发送到直接连接的网络上的主机。
        // 这通常仅供诊断或路由程序使用。此规则仅适用于路由协议族；数据包套接字不适用。
        const DONTROUTE    = 0x4;
        // 让 send()以非阻塞方式执行。如果数据不能立刻传输（因为套接字发送缓冲区已满），那
        // 么该调用不会阻塞，而是调用失败，伴随的错误码为 EAGAIN。和recv()一样，可以通过对套
        // 接字设定 O_NONBLOCK 标记来实现同样的效果。
        const DONTWAIT     = 0x40;
        // 终止记录（当支持此概念时，例如 SOCK_SEQPACKET 类型的套接字）。
        const EOR          = 0x80;
        // 告知链路层已取得进展：您已收到来自另一端的成功回复。如果链路层未收到此回复，
        // 它将定期重新探测邻居（例如，通过单播 ARP）。此功能仅适用于SOCK_DGRAM 和 SOCK_RAW 套接字，
        // 目前仅支持 IPv4 和 IPv6。
        const CONFIRM      = 0x800;
        // 当在已连接的流式套接字上发送数据时，如果连接的另一端已经关闭了，指定该标记后将
        // 不会产生SIGPIPE 信号。相反，send()调用会失败，伴随的错误码为EPIPE。这和忽略SIGPIPE
        // 信号所得到的行为相同。区别在于该标记可以在每次调用中控制信号发送的行为。
        const NOSIGNAL     = 0x4000;
        // 在连续的 send()或 sendto()调用中
        // 传输的数据，如果指定了MSG_MORE 标记，那么数据会打包成一个单独的数据报。仅当下一
        // 次调用中没有指定该标记时数据才会传输出去。
        const MORE         = 0x8000;
    }
}

bitflags! {
    /// 代表接受数据的标志
    ///
    /// See [`SocketOps::recv`].
    #[derive(Default)]
    pub struct RecvFlags: u32 {
        // 在套接字上接收带外数据。
        const OOB = 0x1;
        // 从套接字缓冲区中获取一份请求字节的副本，但不会将请求的字节从缓冲区中实际移除。
        // 这份数据稍后可以由其他的 recv()或 read()调用重新读取。
        const PEEK = 0x2;
        //  recv()以非阻塞方式执行。如果没有数据可用，那么 recv()不会阻塞而是立刻返回，
        // 伴随的错误码为 EAGAIN。我们可以通过 fcntl()把套接字设为非阻塞模式（O_NONBLOCK）
        // 从而达到相同的效果。区别在于 MSG_DONTWAIT 允许我们在每次调用中控制非阻塞行为。
        const DONTWAIT = 0x40;
        // 指定了 MSG_WAITALL 标记后将导致系统调用阻塞，直到成功接收到 length
        // 个字节。
        const WAITALL = 0x100;
        // recvmsg() only
        // 使用 SCM_RIGHTS 操作为通过 UNIX 域文件描述符接收的文件描述符设置
        // close-on-exec 标志。此标志的用途与 open(2) 的 O_CLOEXEC 标志相同
        const CMSG_CLOEXEC = 0x40000000;
        // 返回数据包的真实长度，即使它比传递的缓冲区更长。
        // 仅适用于数据报套接字。
        const TRUNCATE = 0x20;
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
pub enum Socket {
    /// UDP socket.
    Udp(UdpSocket),
    /// TCP socket.
    Tcp(TcpSocket),
    /// Unix domain socket.
    Unix(UnixSocket),
}

impl Configurable for Socket {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> SysResult<bool> {
        match self {
            Socket::Tcp(tcp) => tcp.get_option_inner(opt),
            Socket::Udp(udp) => udp.get_option_inner(opt),
            Socket::Unix(unix) => unix.get_option_inner(opt),
        }
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> SysResult<bool> {
        match self {
            Socket::Tcp(tcp) => tcp.set_option_inner(opt),
            Socket::Udp(udp) => udp.set_option_inner(opt),
            Socket::Unix(unix) => unix.set_option_inner(opt),
        }
    }
}

impl SocketOps for Socket {
    fn bind(&self, local_addr: SocketAddrEx) -> SysResult {
        match self {
            Socket::Tcp(tcp) => tcp.bind(local_addr),
            Socket::Udp(udp) => udp.bind(local_addr),
            Socket::Unix(unix) => unix.bind(local_addr),
        }
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> SysResult {
        match self {
            Socket::Tcp(tcp) => tcp.connect(remote_addr),
            Socket::Udp(udp) => udp.connect(remote_addr),
            Socket::Unix(unix) => unix.connect(remote_addr),
        }
    }

    fn listen(&self) -> SysResult {
        match self {
            Socket::Tcp(tcp) => tcp.listen(),
            Socket::Udp(udp) => udp.listen(),
            Socket::Unix(unix) => unix.listen(),
        }
    }

    fn accept(&self) -> SysResult<Socket> {
        match self {
            Socket::Tcp(tcp) => tcp.accept(),
            Socket::Udp(udp) => udp.accept(),
            Socket::Unix(unix) => unix.accept(),
        }
    }

    fn send(&self, src: UserBuffer, options: SendOptions) -> SysResult<usize> {
        match self {
            Socket::Tcp(tcp) => tcp.send(src, options),
            Socket::Udp(udp) => udp.send(src, options),
            Socket::Unix(unix) => unix.send(src, options),
        }
    }

    fn recv(&self, dst: UserBuffer, options: RecvOptions<'_>) -> SysResult<usize> {
        match self {
            Socket::Tcp(tcp) => tcp.recv(dst, options),
            Socket::Udp(udp) => udp.recv(dst, options),
            Socket::Unix(unix) => unix.recv(dst, options),
        }
    }

    fn local_addr(&self) -> SysResult<SocketAddrEx> {
        match self {
            Socket::Tcp(tcp) => tcp.local_addr(),
            Socket::Udp(udp) => udp.local_addr(),
            Socket::Unix(unix) => unix.local_addr(),
        }
    }

    fn peer_addr(&self) -> SysResult<SocketAddrEx> {
        match self {
            Socket::Tcp(tcp) => tcp.peer_addr(),
            Socket::Udp(udp) => udp.peer_addr(),
            Socket::Unix(unix) => unix.peer_addr(),
        }
    }

    fn shutdown(&self, how: Shutdown) -> SysResult {
        match self {
            Socket::Tcp(tcp) => tcp.shutdown(how),
            Socket::Udp(udp) => udp.shutdown(how),
            Socket::Unix(unix) => unix.shutdown(how),
        }
    }
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
