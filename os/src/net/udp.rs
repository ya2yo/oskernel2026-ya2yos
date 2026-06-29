use alloc::vec;
use alloc::vec::Vec;
use core::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    task::Context,
};
use linux_raw_sys::net::{__kernel_sockaddr_storage, group_source_req};
use log::{debug, info, warn};

use crate::{
    fs::File,
    mm::UserBuffer,
    utils::{SysErrNo, SysResult},
};
use crate::{net::extract_ipaddr_from_sockaddr, syscall::PollEvents};
use smoltcp::{
    iface::{MulticastError, SocketHandle},
    phy::PacketMeta,
    socket::udp::{self as smol, UdpMetadata},
    storage::PacketMetadata,
    wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address},
};
use spin::Mutex;
use spin::RwLock;

use super::{
    check_privileged_port_bind,
    consts::{UDP_RX_BUF_LEN, UDP_TX_BUF_LEN},
    general::GeneralOptions,
    get_service,
    options::{Configurable, GetSocketOption, SetSocketOption},
    poll_interfaces, RecvFlags, RecvOptions, SendOptions, Shutdown, SocketAddrEx, SocketOps,
    SOCKET_SET,
};

pub(crate) fn new_udp_socket() -> smol::Socket<'static> {
    // TODO(mivik): buffer size
    smol::Socket::new(
        smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; UDP_RX_BUF_LEN]),
        smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; UDP_TX_BUF_LEN]),
    )
}

/// A UDP socket that provides POSIX-like APIs.
pub struct UdpSocket {
    handle: SocketHandle,
    local_addr: RwLock<Option<IpEndpoint>>,
    peer_addr: RwLock<Option<(IpEndpoint, IpAddress)>>,

    general: GeneralOptions,
    /// 记录该套接字加入的组播组列表: (接口索引, 组播组地址)
    memberships: RwLock<Vec<(u32, IpAddress)>>,
}

impl UdpSocket {
    /// Creates a new UDP socket.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let socket = new_udp_socket();
        let handle = SOCKET_SET.add(socket);

        Self {
            handle,
            local_addr: RwLock::new(None),
            peer_addr: RwLock::new(None),

            general: GeneralOptions::new(),
            memberships: RwLock::new(Vec::new()),
        }
    }

    fn with_smol_socket<R>(&self, f: impl FnOnce(&mut smol::Socket) -> R) -> R {
        SOCKET_SET.with_socket_mut::<smol::Socket, _, _>(self.handle, f)
    }

    fn remote_endpoint(&self) -> SysResult<(IpEndpoint, IpAddress)> {
        match self.peer_addr.try_read() {
            Some(addr) => addr.ok_or(SysErrNo::ENOTCONN),
            None => Err(SysErrNo::ENOTCONN),
        }
    }
}

impl Configurable for UdpSocket {
    fn get_option_inner(&self, option: &mut GetSocketOption) -> SysResult<bool> {
        use GetSocketOption as O;

        if self.general.get_option_inner(option)? {
            return Ok(true);
        }
        match option {
            O::Ttl(ttl) => {
                self.with_smol_socket(|socket| {
                    **ttl = socket.hop_limit().unwrap_or(64);
                });
            }
            O::SendBuffer(size) => {
                **size = UDP_TX_BUF_LEN;
            }
            O::ReceiveBuffer(size) => {
                **size = UDP_RX_BUF_LEN;
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
            O::Ttl(ttl) => {
                self.with_smol_socket(|socket| {
                    socket.set_hop_limit(Some(*ttl));
                });
            }
            O::JoinGroup(gr) => {
                let group_addr = extract_ipaddr_from_sockaddr(&gr.gr_group)?;
                let if_index = gr.gr_interface;

                // 检查本地是否已加入过
                {
                    let memberships = self.memberships.read();
                    if memberships
                        .iter()
                        .any(|&(idx, addr)| idx == if_index && addr == group_addr)
                    {
                        return Err(SysErrNo::EADDRINUSE);
                    }
                }

                // 先通知 smoltcp（可能因组播表满而失败），成功后再记录到本地列表
                get_service()
                    .iface
                    .join_multicast_group(group_addr)
                    .map_err(|e| match e {
                        MulticastError::GroupTableFull => SysErrNo::ENOBUFS,
                        MulticastError::Unaddressable => SysErrNo::EINVAL,
                    })?;

                self.memberships.write().push((if_index, group_addr));
                debug!(
                    "UDP socket {}: joined multicast group {:?} on if_index {}",
                    self.handle, group_addr, if_index
                );
            }
            O::LeaveGroup(gr) => {
                let group_addr = extract_ipaddr_from_sockaddr(&gr.gr_group)?;
                let if_index = gr.gr_interface;

                let mut memberships = self.memberships.write();
                let pos = memberships
                    .iter()
                    .position(|&(idx, addr)| idx == if_index && addr == group_addr);
                match pos {
                    Some(index) => {
                        memberships.remove(index);
                        drop(memberships);

                        // smoltcp 内部维护 IGMP 状态机，重复 leave 不会出错
                        let _ = get_service().iface.leave_multicast_group(group_addr);
                        debug!(
                            "UDP socket {}: left multicast group {:?} on if_index {}",
                            self.handle, group_addr, if_index
                        );
                    }
                    None => {
                        return Err(SysErrNo::EADDRNOTAVAIL);
                    }
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
impl SocketOps for UdpSocket {
    fn bind(&self, local_addr: SocketAddrEx) -> SysResult {
        let mut local_addr = local_addr.into_ip()?;
        let mut guard = self.local_addr.write();

        if local_addr.port() == 0 {
            local_addr.set_port(get_ephemeral_port()?);
        }
        check_privileged_port_bind(local_addr.port())?;
        if guard.is_some() {
            return Err(SysErrNo::EINVAL);
        }

        let local_endpoint = IpEndpoint::from(local_addr);
        let endpoint = IpListenEndpoint {
            addr: (!local_endpoint.addr.is_unspecified()).then_some(local_endpoint.addr),
            port: local_endpoint.port,
        };

        if !self.general.reuse_address() {
            // Check if the address is already in use
            SOCKET_SET.bind_check(local_endpoint.addr, local_endpoint.port)?;
        }

        self.with_smol_socket(|socket| {
            socket.bind(endpoint).map_err(|e| match e {
                smol::BindError::InvalidState => SysErrNo::EINVAL,
                smol::BindError::Unaddressable => SysErrNo::ECONNREFUSED,
            })
        });
        self.general
            .set_device_mask(get_service().device_mask_for(&endpoint));

        *guard = Some(local_endpoint);
        info!("UDP socket {}: bound on {}", self.handle, endpoint);
        Ok(())
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> SysResult {
        let remote_addr = remote_addr.into_ip()?;
        let mut guard = self.peer_addr.write();
        if self.local_addr.read().is_none() {
            self.bind(SocketAddrEx::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                0,
            )))?;
        }

        let remote_addr = IpEndpoint::from(remote_addr);
        let src = get_service().get_source_address(&remote_addr.addr);
        *guard = Some((remote_addr, src));
        self.with_smol_socket(|socket| socket.set_remote_endpoint(Some(remote_addr)));
        // debug!("UDP socket {}: connected to {}", self.handle, remote_addr);
        Ok(())
    }

    fn send(&self, mut src: UserBuffer, options: SendOptions) -> SysResult<usize> {
        let (remote_addr, source_addr) = match options.to {
            Some(addr) => {
                let addr = IpEndpoint::from(addr.into_ip()?);
                let src = get_service().get_source_address(&addr.addr);
                (addr, src)
            }
            None => self.remote_endpoint()?,
        };
        if remote_addr.port == 0 || remote_addr.addr.is_unspecified() {
            return Err(SysErrNo::EINVAL);
        }

        if self.local_addr.read().is_none() {
            self.bind(SocketAddrEx::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                0,
            )))?;
        }
        self.general.send_poller(self, || {
            poll_interfaces();
            self.with_smol_socket(|socket| {
                if !socket.is_open() {
                    // not connected
                    Err(SysErrNo::ENOTCONN)
                } else if !socket.can_send() {
                    Err(SysErrNo::EAGAIN)
                } else {
                    let buf = socket
                        .send(
                            src.len(),
                            UdpMetadata {
                                endpoint: remote_addr,
                                local_address: Some(source_addr),
                                meta: PacketMeta::default(),
                            },
                        )
                        .map_err(|e| match e {
                            smol::SendError::BufferFull => SysErrNo::EAGAIN,
                            smol::SendError::Unaddressable => SysErrNo::ECONNREFUSED,
                        })?;
                    let read = src.read(buf.len());
                    assert_eq!(read.len(), buf.len());
                    buf.copy_from_slice(&read);
                    Ok(read.len())
                }
            })
        })
    }

    fn recv(&self, mut dst: UserBuffer, options: RecvOptions) -> SysResult<usize> {
        if self.local_addr.read().is_none() {
            return Err(SysErrNo::ENOTCONN);
        }

        enum ExpectedRemote<'a> {
            Any(&'a mut SocketAddrEx),
            Expecting(IpEndpoint),
        }
        let mut expected_remote = match options.from {
            Some(addr) => ExpectedRemote::Any(addr),
            None => ExpectedRemote::Expecting(self.remote_endpoint()?.0),
        };

        self.general.recv_poller(self, || {
            poll_interfaces();
            self.with_smol_socket(|socket| {
                if !socket.is_open() {
                    // not bound
                    Err(SysErrNo::ENOTCONN)
                } else if !socket.can_recv() {
                    Err(SysErrNo::EAGAIN)
                } else {
                    let result = if options.flags.contains(RecvFlags::PEEK) {
                        socket.peek().map(|(data, meta)| (data, *meta))
                    } else {
                        socket.recv()
                    };
                    match result {
                        Ok((src, meta)) => {
                            match &mut expected_remote {
                                ExpectedRemote::Any(remote_addr) => {
                                    **remote_addr = SocketAddrEx::Ip(meta.endpoint.into());
                                }
                                ExpectedRemote::Expecting(expected) => {
                                    if (!expected.addr.is_unspecified()
                                        && expected.addr != meta.endpoint.addr)
                                        || (expected.port != 0
                                            && expected.port != meta.endpoint.port)
                                    {
                                        return Err(SysErrNo::EAGAIN);
                                    }
                                }
                            }

                            let read = dst.write(src);
                            if read < src.len() {
                                warn!("UDP message truncated: {} -> {} bytes", src.len(), read);
                            }

                            Ok(if options.flags.contains(RecvFlags::TRUNCATE) {
                                src.len()
                            } else {
                                read
                            })
                        }
                        Err(smol::RecvError::Exhausted) => Err(SysErrNo::EAGAIN),
                        Err(smol::RecvError::Truncated) => {
                            unreachable!("UDP socket recv never returns Err(Truncated)")
                        }
                    }
                }
            })
        })
    }

    fn local_addr(&self) -> SysResult<SocketAddrEx> {
        match self.local_addr.try_read() {
            Some(addr) => addr
                .map(Into::into)
                .map(SocketAddrEx::Ip)
                .ok_or(SysErrNo::ENOTCONN),
            None => Err(SysErrNo::ENOTCONN),
        }
    }

    fn peer_addr(&self) -> SysResult<SocketAddrEx> {
        self.remote_endpoint()
            .map(|it| it.0.into())
            .map(SocketAddrEx::Ip)
    }

    fn shutdown(&self, _how: Shutdown) -> SysResult {
        // TODO(mivik): shutdown
        poll_interfaces();

        self.with_smol_socket(|socket| {
            // debug!("UDP socket {}: shutting down", self.handle);
            socket.close();
        });
        *self.peer_addr.write() = None;
        Ok(())
    }
}

impl File for UdpSocket {
    fn poll(&self, _e: PollEvents) -> PollEvents {
        poll_interfaces();
        if self.local_addr.read().is_none() {
            return PollEvents::empty();
        }

        let mut events = PollEvents::empty();
        self.with_smol_socket(|socket| {
            events.set(PollEvents::IN, socket.can_recv());
            events.set(PollEvents::OUT, socket.can_send());
        });
        events
    }

    fn register(&self, context: &mut Context<'_>, events: PollEvents) {
        if events.intersects(PollEvents::IN | PollEvents::OUT) {
            self.general.register_waker(context.waker());
        }
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        self.shutdown(Shutdown::Both).ok();
        SOCKET_SET.remove(self.handle);
    }
}

fn get_ephemeral_port() -> SysResult<u16> {
    const PORT_START: u16 = 0xc000;
    const PORT_END: u16 = 0xffff;
    static CURR: Mutex<u16> = Mutex::new(PORT_START);
    let mut curr = CURR.lock();

    let port = *curr;
    if *curr == PORT_END {
        *curr = PORT_START;
    } else {
        *curr += 1;
    }
    Ok(port)
}
