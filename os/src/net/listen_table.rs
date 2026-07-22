use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec};
use core::ops::DerefMut;
use log::{debug, warn};

use crate::utils::{SysErrNo, SysResult};
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp::{self, SocketBuffer, State},
    wire::{IpEndpoint, IpListenEndpoint},
};
use spin::Mutex;

use super::{
    consts::{LISTEN_QUEUE_SIZE, LOOPBACK_TCP_MSS, TCP_RX_BUF_LEN, TCP_TX_BUF_LEN},
    SOCKET_SET,
};

/// 总端口数量 (0-65535)
const PORT_NUM: usize = 65536;

/// 监听表项的内部结构
struct ListenTableEntryInner {
    /// 监听的 IP 和 端口信息
    listen_endpoint: IpListenEndpoint,
    /// 等待被 accept 的 Socket 队列
    syn_queue: VecDeque<SocketHandle>,
}

impl ListenTableEntryInner {
    pub fn new(listen_endpoint: IpListenEndpoint) -> Self {
        Self {
            listen_endpoint,
            // 为队列预分配空间，防止频繁内存分配
            syn_queue: VecDeque::with_capacity(LISTEN_QUEUE_SIZE),
        }
    }
}

/// 当销毁监听项时，必须清理队列中所有尚未被提取的 Socket
impl Drop for ListenTableEntryInner {
    fn drop(&mut self) {
        for &handle in &self.syn_queue {
            SOCKET_SET.remove(handle);
        }
    }
}

/// 使用 Arc<Mutex<Option<...>>> 支持并发访问并允许动态开启/关闭监听
type ListenTableEntry = Arc<Mutex<Option<Box<ListenTableEntryInner>>>>;

/// TCP 监听总表：管理所有 65536 个端口的状态
pub struct ListenTable {
    tcp: Box<[ListenTableEntry]>,
}

impl ListenTable {
    /// 初始化空的监听表
    pub fn new() -> Self {
        let tcp = unsafe {
            let mut buf = Box::new_uninit_slice(PORT_NUM);
            for i in 0..PORT_NUM {
                buf[i].write(Arc::default());
            }
            buf.assume_init()
        };
        Self { tcp }
    }

    /// 检查某个端口是否处于空闲状态
    pub fn can_listen(&self, port: u16) -> bool {
        self.tcp[port as usize].lock().is_none()
    }

    /// 开始监听指定的端口
    pub fn listen(&self, listen_endpoint: IpListenEndpoint) -> SysResult {
        let port = listen_endpoint.port;
        // 端口 0 在 Hello_OS 中是非法地址
        assert_ne!(port, 0);
        let mut entry = self.tcp[port as usize].lock();
        if entry.is_none() {
            // 如果该端口没有被占用，则创建新的监听项
            *entry = Some(Box::new(ListenTableEntryInner::new(listen_endpoint)));
            Ok(())
        } else {
            warn!("socket already listening on port {port}");
            Err(SysErrNo::EADDRINUSE)
        }
    }

    /// 停止对某个端口的监听并释放相关 Socket 资源
    pub fn unlisten(&self, port: u16) {
        // debug!("TCP socket unlisten on {}", port);
        // `ListenTableEntryInner::drop()` removes queued sockets. Drop it only
        // after releasing the port lock, otherwise this path reverses the
        // socket-set -> listen-table order used by packet processing.
        let removed = self.tcp[port as usize].lock().take();
        drop(removed);
    }

    /// 获取对应端口监听项的克隆 (Arc 引用计数增加)
    fn listen_entry(&self, port: u16) -> Arc<Mutex<Option<Box<ListenTableEntryInner>>>> {
        self.tcp[port as usize].clone()
    }

    /// 检查当前队列中是否有已经完成握手、可以被 accept 的连接
    pub fn can_accept(&self, port: u16) -> SysResult<bool> {
        // Packet processing uses SOCKET_SET -> per-port entry. Use the same
        // order here so handles cannot be removed or reused while inspected.
        let sockets = SOCKET_SET.inner.lock();
        let entry = self.listen_entry(port);
        let table = entry.lock();
        let Some(entry) = table.as_ref() else {
            warn!("accept before listen");
            return Err(SysErrNo::EINVAL);
        };
        Ok(entry
            .syn_queue
            .iter()
            .any(|&handle| is_connected(&sockets, handle)))
    }

    /// 从监听队列中提取一个已建立的连接
    pub fn accept(&self, port: u16) -> SysResult<SocketHandle> {
        let sockets = SOCKET_SET.inner.lock();
        let entry = self.listen_entry(port);
        let mut table = entry.lock();
        // 确保该端口确实在监听
        let Some(entry) = table.deref_mut() else {
            warn!("accept before listen");
            return Err(SysErrNo::EINVAL);
        };

        let syn_queue: &mut VecDeque<SocketHandle> = &mut entry.syn_queue;
        // 寻找队列中第一个已经完成连接的 Socket 索引
        let idx = syn_queue
            .iter()
            .position(|&handle| is_connected(&sockets, handle))
            .ok_or(SysErrNo::EAGAIN)?; // wait for connection
        if idx > 0 {
            warn!(
                "slow SYN queue enumeration: index = {}, len = {}!",
                idx,
                syn_queue.len()
            );
        }
        // 从队列中移除该 Socket
        let handle = syn_queue.swap_remove_front(idx).unwrap();
        // 如果在取出的一瞬间连接断开了
        if is_closed(&sockets, handle) {
            warn!("accept failed: connection reset");
            Err(SysErrNo::ECONNRESET)
        } else {
            Ok(handle)
        }
    }

    /// 【协议栈底层调用】当网卡收到 TCP 报文且没有现成 Socket 匹配时，检查是否是发给监听端口的
    pub fn incoming_tcp_packet(
        &self,
        _src: IpEndpoint,
        dst: IpEndpoint,
        sockets: &mut SocketSet<'_>,
    ) {
        let local_loopback = matches!(
            dst.addr,
            smoltcp::wire::IpAddress::Ipv4(addr) if addr.is_loopback()
        );
        // 如果目标端口正在监听
        if let Some(entry) = self.listen_entry(dst.port).lock().deref_mut() {
            // 检查队列（Backlog）是否已满
            if entry.syn_queue.len() >= LISTEN_QUEUE_SIZE {
                warn!("SYN queue overflow!");
                return;
            }
            // 创建一个新的 smoltcp Socket 用来处理这个潜在的新连接
            let mut socket = smoltcp::socket::tcp::Socket::new(
                SocketBuffer::new(vec![0; TCP_RX_BUF_LEN]),
                SocketBuffer::new(vec![0; TCP_TX_BUF_LEN]),
            );
            if local_loopback {
                socket.set_local_mss(Some(LOOPBACK_TCP_MSS));
                // The server writes HTTP headers and body separately. Do not
                // hold the short body behind an unacknowledged header packet.
                socket.set_nagle_enabled(false);
                // The loopback peer is in the same kernel. Send ACKs while
                // handling ingress instead of depending on a delayed-ACK
                // timer shared by unrelated socket waiters.
                socket.set_ack_delay(None);
            }
            // 将新 Socket 设为监听状态，准备响应 SYN
            if let Err(err) = socket.listen(IpListenEndpoint {
                addr: None,
                port: dst.port,
            }) {
                warn!("Failed to listen on {}: {:?}", entry.listen_endpoint, err);
                return;
            }
            // 将 Socket 加入全局管理集合并放入当前端口的待处理队列
            let handle = sockets.add(socket);
            // debug!(
            //     "TCP socket {}: prepare for connection {} -> {}",
            //     handle, src, entry.listen_endpoint
            // );
            entry.syn_queue.push_back(handle);
        }
    }
}

/// 判断 Socket 是否已完成握手（不再处于监听或同步状态）
fn is_connected(sockets: &SocketSet<'_>, handle: SocketHandle) -> bool {
    let socket = sockets.get::<tcp::Socket>(handle);
    !matches!(socket.state(), State::Listen | State::SynReceived)
}

/// 判断 Socket 是否已经彻底关闭
fn is_closed(sockets: &SocketSet<'_>, handle: SocketHandle) -> bool {
    matches!(sockets.get::<tcp::Socket>(handle).state(), State::Closed)
}
