use alloc::vec;
use core::{
    net::{Ipv4Addr, SocketAddr},
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use crate::{mm::UserBuffer, utils::{PollSet, SysErrNo, SysResult}};
use crate::syscall::PollEvents;
use crate::fs::File;
use spin::Mutex;
use smoltcp::{
    iface::SocketHandle,
    socket::tcp as smol,
    time::Duration,
    wire::{IpEndpoint, IpListenEndpoint},
};

use super::{
    LISTEN_TABLE, RecvFlags, RecvOptions, SOCKET_SET, SendOptions, Shutdown, Socket, SocketAddrEx,
    SocketOps,
    consts::{TCP_RX_BUF_LEN, TCP_TX_BUF_LEN},
    general::GeneralOptions,
    get_service,
    options::{Configurable, GetSocketOption, SetSocketOption},
    poll_interfaces,
    state::*,
};
/// 创建新的tcp套接字
/// 分配接收和发送缓冲区，缓冲区大小由常量定义
pub(crate) fn new_tcp_socket() -> smol::Socket<'static> {
    smol::Socket::new(
        smol::SocketBuffer::new(vec![0; TCP_RX_BUF_LEN]),
        smol::SocketBuffer::new(vec![0; TCP_TX_BUF_LEN]),
    )
}

/// Tcp 套接字结构体，封装了状态管理、套接字句柄及各种选项
pub struct TcpSocket {
    /// 套接字当前状态的锁
    state: StateLock,
    /// 指向全局 SOCKET_SET 中实际 smoltcp 套接字的句柄
    handle: SocketHandle,
    /// 通用套接字选项
    general: GeneralOptions,
    /// 标志位：接收端是否已关闭
    rx_closed: AtomicBool,
    /// 用于处理接收端关闭时的唤醒和轮询
    poll_rx_closed: PollSet,
}

unsafe impl Sync for TcpSocket {}

impl TcpSocket {
    /// 创建一个新的 TCP 套接字，初始状态为 Idle
    pub fn new() -> Self {
        Self {
            state: StateLock::new(State::Idle),
            handle: SOCKET_SET.add(new_tcp_socket()),

            general: GeneralOptions::new(),
            rx_closed: AtomicBool::new(false),
            poll_rx_closed: PollSet::new(),
        }
    }

    /// 根据已有的句柄创建一个已连接的 TCP 套接字，通常由 accept 调用
    fn new_connected(handle: SocketHandle) -> Self {
        let result = Self {
            state: StateLock::new(State::Connected),
            handle,

            general: GeneralOptions::new(),
            rx_closed: AtomicBool::new(false),
            poll_rx_closed: PollSet::new(),
        };
        // 获取该套接字绑定的端点，并设置相应的网络设备掩码
        result.with_smol_socket(|socket| {
            result
                .general
                .set_device_mask(get_service().device_mask_for(&socket.get_bound_endpoint()));
        });
        result
    }
}

impl Default for TcpSocket {
    fn default() -> Self {
        Self::new()
    }
}

/// 内部辅助方法
impl TcpSocket {
    /// 获取当前套接字状态
    fn state(&self) -> State {
        self.state.get()
    }
    /// 判断是否处于监听状态
    #[inline]
    fn is_listening(&self) -> bool {
        self.state() == State::Listening
    }
    /// 安全地获取并操作全局 SOCKET_SET 中对应的 smoltcp 套接字
    fn with_smol_socket<R>(&self, f: impl FnOnce(&mut smol::Socket) -> R) -> R {
        SOCKET_SET.with_socket_mut::<smol::Socket, _, _>(self.handle, f)
    }
    /// 获取当前绑定的本地端点的IP和端口
    fn bound_endpoint(&self) -> SysResult<IpListenEndpoint> {
        let endpoint = self.with_smol_socket(|socket| socket.get_bound_endpoint());
        if endpoint.port == 0 {
            // ax_bail!(InvalidInput, "not bound");
        }
        Ok(endpoint)
    }
    /// 轮询连接状态：用于 connect 操作时确认是否握手成功
    fn poll_connect(&self) -> PollEvents {
        let mut events = PollEvents::empty();
        let writable = self.with_smol_socket(|socket| match socket.state() {
            smol::State::SynSent => false, // 还在发送 SYN，未连接
            smol::State::Established => {
                self.state.set(State::Connected); // 连接成功
                debug!(
                    "TCP socket {}: connected to {}",
                    self.handle,
                    socket.remote_endpoint().unwrap(),
                );
                true
            }
            _ => {
                self.state.set(State::Closed); // 连接失败
                true
            }
        });
        events.set(PollEvents::OUT, writable);// 连接成功，该套接字现在可写
        events
    }
    /// 轮询常规数据流状态
    fn poll_stream(&self) -> PollEvents {
        let mut events = PollEvents::empty();
        self.with_smol_socket(|socket| {
            // 可读要求接收未关闭且套接字已不能接收更多数据 或 缓冲区有数据
            events.set(
                PollEvents::IN,
                !self.rx_closed.load(Ordering::Acquire)
                    && (!socket.may_recv() || socket.can_recv()),
            );
            // 可写要求发送缓冲区有剩余空间
            events.set(PollEvents::OUT, !socket.may_send() || socket.can_send());
        });
        events
    }
    /// 轮询监听状态，检查是否有待处理的连接
    fn poll_listener(&self) -> PollEvents {
        let mut events = PollEvents::empty();
        events.set(
            PollEvents::IN,
            LISTEN_TABLE
                .can_accept(self.bound_endpoint().unwrap().port)
                .unwrap(),
        );
        events
    }
}
/// 实现套接字选项配置接口
impl Configurable for TcpSocket {
    fn get_option_inner(&self, option: &mut GetSocketOption) -> SysResult<bool> {
        use GetSocketOption as O;
        // 优先处理通用选项
        if self.general.get_option_inner(option)? {
            return Ok(true);
        }

        match option {
            O::NoDelay(no_delay) => {
                **no_delay = self.with_smol_socket(|socket| !socket.nagle_enabled());
            }
            O::KeepAlive(keep_alive) => {
                **keep_alive = self.with_smol_socket(|socket| socket.keep_alive().is_some());
            }
            O::MaxSegment(max_segment) => {
                **max_segment = 1460;// 默认 MSS
            }
            O::SendBuffer(size) => {
                **size = TCP_TX_BUF_LEN;
            }
            O::ReceiveBuffer(size) => {
                **size = TCP_RX_BUF_LEN;
            }
            O::TcpInfo(_) => {
                // TODO(mivik): implement TCP_INFO
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn set_option_inner(&self, option: SetSocketOption) -> SysResult<bool> {
        use SetSocketOption as O;

        if self.general.set_option_inner(option)? {
            return Ok(true);
        }

        match option {
            O::NoDelay(no_delay) => {
                self.with_smol_socket(|socket| {
                    socket.set_nagle_enabled(!no_delay);
                });
            }
            O::KeepAlive(keep_alive) => {
                self.with_smol_socket(|socket| {
                    socket.set_keep_alive(keep_alive.then(|| Duration::from_secs(75)));
                });
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
/// 实现核心套接字操作
impl SocketOps for TcpSocket {
    /// 绑定本地地址和端口
    fn bind(&self, local_addr: SocketAddrEx) -> SysResult {
        let mut local_addr = local_addr.into_ip()?;
        self.state
            .lock(State::Idle)// 只有 Idle 状态可以绑定
            .map_err(|_| return Err(SysErrNo::EINVAL))?
            .transit(State::Idle, || {
                // 如果没指定端口，则自动分配一个临时端口
                if local_addr.port() == 0 {
                    local_addr.set_port(get_ephemeral_port()?);
                }
                // 检查端口是否被占用
                if !self.general.reuse_address() {
                    SOCKET_SET.bind_check(local_addr.ip().into(), local_addr.port())?;
                }

                self.with_smol_socket(|socket| {
                    if socket.get_bound_endpoint().port != 0 {
                        return Err(SysErrNo::InvalidInput);
                    }
                    let endpoint = IpListenEndpoint {
                        addr: if local_addr.ip().is_unspecified() {
                            None
                        } else {
                            Some(local_addr.ip().into())
                        },
                        port: local_addr.port(),
                    };
                    socket.set_bound_endpoint(endpoint);
                    // 更新绑定的网络设备
                    self.general
                        .set_device_mask(get_service().device_mask_for(&endpoint));
                    Ok(())
                })?;
                debug!("TCP socket {}: binding to {}", self.handle, local_addr);
                Ok(())
            })
    }

    /// 发起连接
    fn connect(&self, remote_addr: SocketAddrEx) -> SysResult {
        let remote_addr = remote_addr.into_ip()?;
        self.state
            .lock(State::Idle)// 状态机检查
            .map_err(|state| {
                if state == State::Connecting {
                    SysErrNo::InProgress
                } else {
                    // TODO(mivik): error code
                    // ax_err_type!(AlreadyConnected)
                }
            })?
            .transit(State::Connecting, || {
                // TODO: check remote addr unreachable
                // let (bound_endpoint, remote_endpoint) = self.get_endpoint_pair(remote_addr)?;
                let remote_endpoint = IpEndpoint::from(remote_addr);
                let mut bound_endpoint =
                    self.with_smol_socket(|socket| socket.get_bound_endpoint());
                // 如果未显式绑定 IP，则自动获取合适的源 IP
                if bound_endpoint.addr.is_none() {
                    bound_endpoint.addr =
                        Some(get_service().get_source_address(&remote_endpoint.addr));
                }
                // 如果未显式绑定端口，分配临时端口
                if bound_endpoint.port == 0 {
                    bound_endpoint.port = get_ephemeral_port()?;
                }
                info!(
                    "TCP connection from {} to {}",
                    bound_endpoint, remote_endpoint
                );

                self.with_smol_socket(|socket| {
                    socket.set_bound_endpoint(bound_endpoint);
                    self.general
                        .set_device_mask(get_service().device_mask_for(&bound_endpoint));
                    // 开启 smoltcp 连接流程
                    socket
                        .connect(
                            get_service().iface.context(),
                            remote_endpoint,
                            bound_endpoint,
                        )
                        .map_err(|e| match e {
                            smol::ConnectError::InvalidState => {
                                Err(SysErrNo::EALREADY)
                            }
                            smol::ConnectError::Unaddressable => {
                                Err(SysErrNo::ECONNREFUSED)
                            }
                        })?;
                    Ok(())
                })
            })?;

        // 出让 CPU 尝试给协议栈处理时间
        // axtask::yield_now();

        // 循环等待连接结果或阻塞
        self.general.send_poller(self, || {
            poll_interfaces();// 驱动网卡收发
            let events = self.poll_connect();
            if !events.contains(PollEvents::OUT) {
                Err(SysErrNo::EAGAIN)
            } else if self.state() == State::Connected {
                Ok(())
            } else {
                Err(SysErrNo::ECONNREFUSED)
            }
        })
    }
    /// 开始监听入站连接
    fn listen(&self) -> SysResult {
        if let Ok(guard) = self.state.lock(State::Idle) {
            guard.transit(State::Listening, || {
                let bound_endpoint = self.with_smol_socket(|socket| socket.get_bound_endpoint());
                // 将端口加入全局监听表
                LISTEN_TABLE.listen(bound_endpoint)?;
                debug!("listening on {}", bound_endpoint);
                Ok(())
            })?;
        } else {
            // ignore simultaneous `listen`s.
        }
        Ok(())
    }
    /// 接受一个新的连接请求
    fn accept(&self) -> SysResult<Socket> {
        if !self.is_listening() {
            return Err(SysErrNo::EINVAL)
        }

        let bound_port = self.bound_endpoint()?.port;
        // 轮询检查是否有新句柄被放入监听表
        self.general.recv_poller(self, || {
            poll_interfaces();
            LISTEN_TABLE.accept(bound_port).map(|handle| {
                let socket = TcpSocket::new_connected(handle);
                debug!(
                    "accepted connection from {}, {}",
                    handle,
                    socket.with_smol_socket(|socket| socket.remote_endpoint().unwrap())
                );
                Socket::Tcp(socket)
            })
        })
    }
    /// 发送数据
    fn send(&self, mut src: impl File, _options: SendOptions) -> SysResult<usize> {
        // SAFETY: `self.handle` should be initialized in a connected socket.
        self.general.send_poller(self, || {
            poll_interfaces();
            self.with_smol_socket(|socket| {
                if !socket.is_active() {
                    Err(SysErrNo::ENOTCONN)
                } else if !socket.can_send() {
                    Err(SysErrNo::EAGAIN)// 发送缓冲区满
                } else {
                    // 将数据从 src 读取并填充到 smoltcp 的发送缓冲区
                    let len = socket
                        .send(|buffer| {
                            let result = src.read(UserBuffer::from(buffer));
                            let len = result.unwrap_or(0);
                            (len, result)
                        })
                        .map_err(|_| Err(SysErrNo::ENOTCONN))??;
                    Ok(len)
                }
            })
        })
    }
    /// 接收数据
    fn recv(&self, mut dst: impl File, options: RecvOptions<'_>) -> SysResult<usize> {
        if self.rx_closed.load(Ordering::Acquire) {
            return Err(SysErrNo::ENOTCONN);
        }
        self.general.recv_poller(self, || {
            poll_interfaces();
            self.with_smol_socket(|socket| {
                if !socket.is_active() {
                    Err(SysErrNo::ENOTCONN)
                } else if !socket.may_recv() {
                    Ok(0)// 对方关闭了发送
                } else if socket.recv_queue() == 0 {
                    Err(SysErrNo::EAGAIN)// 暂无数据
                } else if options.flags.contains(RecvFlags::PEEK) {
                    // PEEK 模式,只读不删
                    dst.write(UserBuffer::from(
                        socket
                            .peek(dst.remaining_mut())
                            .map_err(|_| Err(SysErrNo::ENOTCONN))?),
                    )
                } else {
                    // 正常接收,将数据从缓冲区移动到 dst
                    socket
                        .recv(|buf| {
                            let result = dst.write(UserBuffer::from(buf));
                            let len = result.unwrap_or(0);
                            (len, result)
                        })
                        .map_err(|_| Err(SysErrNo::ENOTCONN))?
                }
            })
        })
    }
    /// 获取本地地址
    fn local_addr(&self) -> SysResult<SocketAddrEx> {
        self.with_smol_socket(|socket| {
            let endpoint = socket.get_bound_endpoint();
            Ok(SocketAddrEx::Ip(SocketAddr::new(
                endpoint
                    .addr
                    .map_or_else(|| Ipv4Addr::UNSPECIFIED.into(), Into::into),
                endpoint.port,
            )))
        })
    }
    /// 获取远端地址
    fn peer_addr(&self) -> SysResult<SocketAddrEx> {
        self.with_smol_socket(|socket| {
            Ok(SocketAddrEx::Ip(
                socket
                    .remote_endpoint()
                    .ok_or(SysErrNo::ENOTCONN)?
                    .into(),
            ))
        })
    }
    /// 关闭套接字的读、写或全部
    fn shutdown(&self, how: Shutdown) -> SysResult {
        // TODO(mivik): shutdown
        if how.has_read() {
            self.rx_closed.store(true, Ordering::Release);
            self.poll_rx_closed.wake();
        }

        // 处理连接状态下的关闭（发送 FIN）
        if let Ok(guard) = self.state.lock(State::Connected) {
            guard.transit(State::Closed, || {
                if how.has_write() {
                    self.with_smol_socket(|socket| {
                        debug!("TCP socket {}: shutting down", self.handle);
                        socket.close();// smoltcp 发起关闭流程
                    });
                }
                poll_interfaces();
                Ok(())
            })?;
        }

        // 处理监听状态下的关闭
        if let Ok(guard) = self.state.lock(State::Listening) {
            guard.transit(State::Closed, || {
                LISTEN_TABLE.unlisten(self.bound_endpoint()?.port);
                poll_interfaces();
                Ok(())
            })?;
        }

        // ignore for other states
        Ok(())
    }
}
/// 实现 File 接口，满足Unix哲学
impl File for TcpSocket {
    fn read(&self, buf: crate::mm::UserBuffer) -> crate::utils::SyscallRet {
        self.recv(buf, RecvOptions::default())
    }
    fn write(&self, buf: crate::mm::UserBuffer) -> crate::utils::SyscallRet {
        self.send(buf, SendOptions::default())
    }
    fn poll(&self, _events:PollEvents) -> PollEvents {
        poll_interfaces();
        let mut events = match self.state() {
            State::Connecting => self.poll_connect(),
            State::Connected | State::Idle | State::Closed => self.poll_stream(),
            State::Listening => self.poll_listener(),
            State::Busy => PollEvents::empty(),
        };
        events.set(PollEvents::RDHUP, self.rx_closed.load(Ordering::Acquire));
        events
    }

    fn register(&self, context: &mut Context<'_>, events: PollEvents) {
        if events.intersects(PollEvents::IN | PollEvents::OUT | PollEvents::RDHUP) {
            self.general.register_waker(context.waker());
        }
        if events.contains(PollEvents::RDHUP) {
            self.poll_rx_closed.register(context.waker());
        }
    }
}
/// 当 TcpSocket 对象离开生命周期时触发
impl Drop for TcpSocket {
    fn drop(&mut self) {
        // 尝试优雅关闭
        if let Err(err) = self.shutdown(Shutdown::Both) {
            warn!("TCP socket {}: shutdown failed: {}", self.handle, err);
        }
        // 从全局句柄池中移除
        SOCKET_SET.remove(self.handle);
        // 再次驱动网卡，确保最后的 FIN 包等控制信息能发出去
        poll_interfaces();
    }
}
/// 辅助函数,分配一个临时的本地端口
fn get_ephemeral_port() -> SysResult<u16> {
    const PORT_START: u16 = 0xc000;// 49152
    const PORT_END: u16 = 0xffff;// 65535
    static CURR: Mutex<u16> = Mutex::new(PORT_START);

    let mut curr = CURR.lock();
    let mut tries = 0;
    // TODO: more robust
    while tries <= PORT_END - PORT_START {
        let port = *curr;
        if *curr == PORT_END {
            *curr = PORT_START;
        } else {
            *curr += 1;
        }
        // 检查端口是否在监听表中已被使用
        if LISTEN_TABLE.can_listen(port) {
            return Ok(port);
        }
        tries += 1;
    }
    Ok(1)// 如果全满了，返回 1 或报错
}
