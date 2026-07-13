use alloc::{
    collections::VecDeque,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::task::Context;

use hashbrown::HashMap;
use spin::{Lazy, Mutex};

use crate::{
    fs::{open, superblock_root_inode, File, FsIndex, InodeType, OpenFlags, NONE_MODE},
    mm::UserBuffer,
    net::{
        options::{Configurable, GetSocketOption, SetSocketOption, UnixCredentials},
        RecvFlags, RecvOptions, SendOptions, Shutdown, Socket, SocketAddrEx, SocketOps,
    },
    syscall::PollEvents,
    task::{block_on, current_task, poll_io},
    utils::{get_abs_path, rsplit_once, PollSet, SysErrNo, SysResult},
};

const UNIX_BUF_SIZE: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum UnixSocketAddr {
    Unnamed,
    Abstract(Vec<u8>),
    Path(String),
}

impl Default for UnixSocketAddr {
    fn default() -> Self {
        Self::Unnamed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnixSocketKind {
    Stream,
    Dgram,
    SeqPacket,
}

struct UnixMessage {
    data: Vec<u8>,
    sender: UnixSocketAddr,
}

struct UnixRecvQueue {
    messages: VecDeque<UnixMessage>,
    queued_bytes: usize,
    closed: bool,
}

impl UnixRecvQueue {
    fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            queued_bytes: 0,
            closed: false,
        }
    }
}

enum QueuePushError {
    Closed,
    Full,
}

struct UnixSocketWriteWaiter(Arc<UnixSocketInner>);

struct UnixSocketInner {
    kind: UnixSocketKind,
    local_addr: Mutex<UnixSocketAddr>,
    peer_addr: Mutex<UnixSocketAddr>,
    peer: Mutex<Option<Weak<UnixSocketInner>>>,
    recv_queue: Mutex<UnixRecvQueue>,
    pending: Mutex<VecDeque<Arc<UnixSocketInner>>>,
    recv_poll: PollSet,
    write_poll: PollSet,
    accept_poll: PollSet,
    listening: Mutex<bool>,
    nonblocking: Mutex<bool>,
    send_closed: Mutex<bool>,
    pid: u32,
}

impl UnixSocketInner {
    fn new(kind: UnixSocketKind, pid: u32) -> Self {
        Self {
            kind,
            local_addr: Mutex::new(UnixSocketAddr::Unnamed),
            peer_addr: Mutex::new(UnixSocketAddr::Unnamed),
            peer: Mutex::new(None),
            recv_queue: Mutex::new(UnixRecvQueue::new()),
            pending: Mutex::new(VecDeque::new()),
            recv_poll: PollSet::new(),
            write_poll: PollSet::new(),
            accept_poll: PollSet::new(),
            listening: Mutex::new(false),
            nonblocking: Mutex::new(false),
            send_closed: Mutex::new(false),
            pid,
        }
    }

    fn try_enqueue(&self, message: UnixMessage) -> Result<(), QueuePushError> {
        let mut queue = self.recv_queue.lock();
        if queue.closed {
            return Err(QueuePushError::Closed);
        }
        if message.data.len() > UNIX_BUF_SIZE - queue.queued_bytes {
            return Err(QueuePushError::Full);
        }
        queue.queued_bytes += message.data.len();
        queue.messages.push_back(message);
        Ok(())
    }

    fn available_write_bytes(&self) -> Result<usize, QueuePushError> {
        let queue = self.recv_queue.lock();
        if queue.closed {
            return Err(QueuePushError::Closed);
        }
        Ok(UNIX_BUF_SIZE - queue.queued_bytes)
    }

    fn recv_closed(&self) -> bool {
        self.recv_queue.lock().closed
    }

    fn close_recv(&self) {
        let dropped = {
            let mut queue = self.recv_queue.lock();
            queue.closed = true;
            queue.queued_bytes = 0;
            core::mem::take(&mut queue.messages)
        };
        drop(dropped);
        self.recv_poll.wake();
        self.write_poll.wake();
    }
}

impl File for UnixSocketWriteWaiter {
    fn poll(&self, _events: PollEvents) -> PollEvents {
        if self.0.available_write_bytes().is_ok_and(|space| space != 0) {
            PollEvents::OUT | PollEvents::WRNORM
        } else {
            PollEvents::empty()
        }
    }

    fn register(&self, context: &mut Context<'_>, events: PollEvents) {
        if events.intersects(PollEvents::OUT | PollEvents::WRNORM) {
            self.0.write_poll.register(context.waker());
        }
    }
}

static UNIX_BINDS: Lazy<Mutex<HashMap<UnixSocketAddr, Arc<UnixSocketInner>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn bind_path_abs(path: &str) -> SysResult<String> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let cwd = task.process.fs_info.get_cwd();
    let abs_path = get_abs_path(&cwd, path);
    let (parent_path, _) = rsplit_once(abs_path.as_str(), "/");
    open(
        parent_path,
        OpenFlags::O_RDONLY | OpenFlags::O_DIRECTORY,
        NONE_MODE,
    )?;
    Ok(abs_path)
}

fn create_path_socket_node(path: &str) -> SysResult {
    let abs_path = bind_path_abs(path)?;
    match open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE) {
        Ok(_) => return Err(SysErrNo::EADDRINUSE),
        Err(SysErrNo::ENOENT) => {}
        Err(err) => return Err(err),
    }

    let inode = superblock_root_inode().create(&abs_path, InodeType::Socket)?;
    inode.fmode_set(InodeType::Socket.mode_bits() | 0o777)?;
    FsIndex::insert_inode_idx(&abs_path, inode);
    FsIndex::insert_special_node_type(&abs_path, InodeType::Socket);
    Ok(())
}

#[derive(Clone)]
pub struct UnixSocket {
    inner: Arc<UnixSocketInner>,
}

impl UnixSocket {
    pub fn new(kind: UnixSocketKind) -> Self {
        let pid = current_task().map_or(0, |task| task.pid() as u32);
        Self {
            inner: Arc::new(UnixSocketInner::new(kind, pid)),
        }
    }

    pub fn new_stream() -> Self {
        Self::new(UnixSocketKind::Stream)
    }

    pub fn new_dgram() -> Self {
        Self::new(UnixSocketKind::Dgram)
    }

    pub fn new_seqpacket() -> Self {
        Self::new(UnixSocketKind::SeqPacket)
    }

    pub fn new_stream_pair() -> (Self, Self) {
        Self::new_pair(UnixSocketKind::Stream)
    }

    pub fn new_dgram_pair() -> (Self, Self) {
        Self::new_pair(UnixSocketKind::Dgram)
    }

    pub fn new_seqpacket_pair() -> (Self, Self) {
        Self::new_pair(UnixSocketKind::SeqPacket)
    }

    fn is_connection_oriented(&self) -> bool {
        matches!(
            self.kind(),
            UnixSocketKind::Stream | UnixSocketKind::SeqPacket
        )
    }

    fn new_pair(kind: UnixSocketKind) -> (Self, Self) {
        let pid = current_task().map_or(0, |task| task.pid() as u32);
        let left = Arc::new(UnixSocketInner::new(kind, pid));
        let right = Arc::new(UnixSocketInner::new(kind, pid));
        *left.peer.lock() = Some(Arc::downgrade(&right));
        *right.peer.lock() = Some(Arc::downgrade(&left));
        (Self { inner: left }, Self { inner: right })
    }

    fn kind(&self) -> UnixSocketKind {
        self.inner.kind
    }

    fn peer(&self) -> SysResult<Arc<UnixSocketInner>> {
        self.inner
            .peer
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(SysErrNo::ENOTCONN)
    }

    fn peer_closed(&self) -> bool {
        self.inner
            .peer
            .lock()
            .as_ref()
            .is_some_and(|peer| peer.upgrade().is_none())
    }

    fn bound_peer(addr: &UnixSocketAddr) -> SysResult<Arc<UnixSocketInner>> {
        UNIX_BINDS
            .lock()
            .get(addr)
            .cloned()
            .ok_or(SysErrNo::ECONNREFUSED)
    }
}

impl Configurable for UnixSocket {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> SysResult<bool> {
        match opt {
            GetSocketOption::SendBuffer(size) | GetSocketOption::ReceiveBuffer(size) => {
                **size = UNIX_BUF_SIZE;
            }
            GetSocketOption::NonBlocking(nonblocking) => {
                **nonblocking = *self.inner.nonblocking.lock();
            }
            GetSocketOption::PeerCredentials(cred) => {
                **cred = UnixCredentials::new(self.peer().map_or(self.inner.pid, |peer| peer.pid));
            }
            GetSocketOption::PassCredentials(_) => {}
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> SysResult<bool> {
        match opt {
            SetSocketOption::NonBlocking(nonblocking) => {
                *self.inner.nonblocking.lock() = *nonblocking;
            }
            SetSocketOption::PassCredentials(_) => {}
            _ => return Ok(false),
        }
        Ok(true)
    }
}

impl SocketOps for UnixSocket {
    fn bind(&self, local_addr: SocketAddrEx) -> SysResult {
        let local_addr = local_addr.into_unix()?;
        if matches!(local_addr, UnixSocketAddr::Unnamed) {
            return Err(SysErrNo::EINVAL);
        }
        let mut current = self.inner.local_addr.lock();
        if !matches!(*current, UnixSocketAddr::Unnamed) {
            return Err(SysErrNo::EINVAL);
        }

        if UNIX_BINDS.lock().contains_key(&local_addr) {
            return Err(SysErrNo::EADDRINUSE);
        }
        if let UnixSocketAddr::Path(path) = &local_addr {
            create_path_socket_node(path)?;
        }
        let mut binds = UNIX_BINDS.lock();
        if binds.contains_key(&local_addr) {
            return Err(SysErrNo::EADDRINUSE);
        }
        binds.insert(local_addr.clone(), self.inner.clone());
        *current = local_addr;
        Ok(())
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> SysResult {
        let remote_addr = remote_addr.into_unix()?;
        let remote = Self::bound_peer(&remote_addr)?;
        if remote.kind != self.kind() {
            return Err(SysErrNo::ECONNREFUSED);
        }
        if self.inner.peer.lock().is_some() {
            return Err(SysErrNo::EISCONN);
        }

        match self.kind() {
            UnixSocketKind::Stream | UnixSocketKind::SeqPacket => {
                if !*remote.listening.lock() {
                    return Err(SysErrNo::ECONNREFUSED);
                }
                let server = Arc::new(UnixSocketInner::new(self.kind(), remote.pid));
                let local = self.inner.local_addr.lock().clone();
                *server.local_addr.lock() = remote_addr.clone();
                *server.peer_addr.lock() = local.clone();
                *server.peer.lock() = Some(Arc::downgrade(&self.inner));
                *self.inner.peer.lock() = Some(Arc::downgrade(&server));
                *self.inner.peer_addr.lock() = remote_addr;
                remote.pending.lock().push_back(server);
                remote.accept_poll.wake();
            }
            UnixSocketKind::Dgram => {
                *self.inner.peer.lock() = Some(Arc::downgrade(&remote));
                *self.inner.peer_addr.lock() = remote_addr;
            }
        }
        Ok(())
    }

    fn listen(&self) -> SysResult {
        if !self.is_connection_oriented() {
            return Err(SysErrNo::EOPNOTSUPP);
        }
        if matches!(*self.inner.local_addr.lock(), UnixSocketAddr::Unnamed) {
            return Err(SysErrNo::EINVAL);
        }
        *self.inner.listening.lock() = true;
        Ok(())
    }

    fn accept(&self) -> SysResult<Socket> {
        if !self.is_connection_oriented() || !*self.inner.listening.lock() {
            return Err(SysErrNo::EINVAL);
        }
        let accepted = block_on(poll_io(
            self,
            PollEvents::IN,
            *self.inner.nonblocking.lock(),
            || {
                self.inner
                    .pending
                    .lock()
                    .pop_front()
                    .ok_or(SysErrNo::EAGAIN)
            },
        ))?;
        Ok(Socket::Unix(UnixSocket { inner: accepted }))
    }

    fn send(&self, mut src: UserBuffer, options: SendOptions) -> SysResult<usize> {
        if *self.inner.send_closed.lock() {
            return Err(SysErrNo::EPIPE);
        }
        let len = src.len();
        let target = if let Some(addr) = options.to {
            let addr = addr.into_unix()?;
            let remote = Self::bound_peer(&addr)?;
            if remote.kind != self.kind() {
                return Err(SysErrNo::ECONNREFUSED);
            }
            remote
        } else {
            self.peer()?
        };
        let sender = self.inner.local_addr.lock().clone();
        let nonblocking = *self.inner.nonblocking.lock()
            || options.flags.contains(crate::net::SendFlags::DONTWAIT);
        let write_waiter = UnixSocketWriteWaiter(target.clone());

        let sent = block_on(poll_io(&write_waiter, PollEvents::OUT, nonblocking, || {
            if *self.inner.send_closed.lock() {
                return Err(SysErrNo::EPIPE);
            }
            let available = match target.available_write_bytes() {
                Ok(available) => available,
                Err(QueuePushError::Closed) => return Err(SysErrNo::EPIPE),
                Err(QueuePushError::Full) => unreachable!(),
            };
            if len == 0 {
                return Ok(0);
            }
            let write_len = match self.kind() {
                UnixSocketKind::Stream => len.min(available),
                UnixSocketKind::Dgram | UnixSocketKind::SeqPacket => {
                    if len > UNIX_BUF_SIZE {
                        return Err(SysErrNo::EMSGSIZE);
                    }
                    if len > available {
                        return Err(SysErrNo::EAGAIN);
                    }
                    len
                }
            };
            if write_len == 0 {
                return Err(SysErrNo::EAGAIN);
            }
            let message = UnixMessage {
                data: src.read(write_len),
                sender: sender.clone(),
            };
            match target.try_enqueue(message) {
                Ok(()) => Ok(write_len),
                Err(QueuePushError::Closed) => Err(SysErrNo::EPIPE),
                Err(QueuePushError::Full) => Err(SysErrNo::EAGAIN),
            }
        }))?;
        target.recv_poll.wake();
        Ok(sent)
    }

    fn recv(&self, mut dst: UserBuffer, mut options: RecvOptions<'_>) -> SysResult<usize> {
        if self.inner.recv_closed() {
            return Ok(0);
        }
        let nonblocking =
            *self.inner.nonblocking.lock() || options.flags.contains(RecvFlags::DONTWAIT);
        let received = block_on(poll_io(self, PollEvents::IN, nonblocking, || {
            let mut queue = self.inner.recv_queue.lock();
            if queue.closed {
                return Ok(0);
            }
            let mut message = match queue.messages.pop_front() {
                Some(message) => message,
                None => {
                    if self.is_connection_oriented() && self.peer_closed() {
                        return Ok(0);
                    }
                    return Err(SysErrNo::EAGAIN);
                }
            };
            let message_len = message.data.len();
            let written = dst.write(&message.data);
            let remaining = if self.kind() == UnixSocketKind::Stream && written < message_len {
                message.data = message.data[written..].to_vec();
                message.data.len()
            } else {
                if let Some(from) = options.from.as_deref_mut() {
                    *from = SocketAddrEx::Unix(message.sender.clone());
                }
                if options.flags.contains(RecvFlags::TRUNCATE) {
                    queue.queued_bytes -= message_len;
                    return Ok(message_len);
                }
                0
            };
            queue.queued_bytes -= message_len - remaining;
            if remaining != 0 {
                queue.messages.push_front(message);
            }
            Ok(written)
        }))?;
        self.inner.write_poll.wake();
        Ok(received)
    }

    fn local_addr(&self) -> SysResult<SocketAddrEx> {
        Ok(SocketAddrEx::Unix(self.inner.local_addr.lock().clone()))
    }

    fn peer_addr(&self) -> SysResult<SocketAddrEx> {
        // 检查socket是否已连接（通过connect或socketpair）
        // 即使peer_addr尚未绑定（如socketpair两端都是unnamed），
        // 只要peer连接存在，getpeername也应该成功返回。
        if self.inner.peer.lock().is_none() {
            return Err(SysErrNo::ENOTCONN);
        }
        let peer = self.inner.peer_addr.lock().clone();
        Ok(SocketAddrEx::Unix(peer))
    }

    fn shutdown(&self, how: Shutdown) -> SysResult {
        if how.has_read() {
            self.inner.close_recv();
        }
        if how.has_write() {
            *self.inner.send_closed.lock() = true;
            if let Ok(peer) = self.peer() {
                peer.recv_poll.wake();
            }
        }
        Ok(())
    }
}

impl crate::fs::File for UnixSocket {
    fn poll(&self, _events: PollEvents) -> PollEvents {
        let queue = self.inner.recv_queue.lock();
        let readable = !queue.messages.is_empty()
            || !self.inner.pending.lock().is_empty()
            || queue.closed
            || (self.is_connection_oriented() && self.peer_closed());
        drop(queue);
        let writable = !*self.inner.send_closed.lock()
            && self.peer().map_or(true, |peer| {
                peer.available_write_bytes().is_ok_and(|space| space != 0)
            });
        let mut events = PollEvents::empty();
        if readable {
            events |= PollEvents::IN | PollEvents::RDNORM;
        }
        if writable {
            events |= PollEvents::OUT | PollEvents::WRNORM;
        }
        events
    }

    fn register(&self, context: &mut Context<'_>, events: PollEvents) {
        if events.intersects(PollEvents::IN | PollEvents::RDNORM) {
            self.inner.recv_poll.register(context.waker());
            self.inner.accept_poll.register(context.waker());
        }
        if events.intersects(PollEvents::OUT | PollEvents::WRNORM) {
            if let Ok(peer) = self.peer() {
                peer.write_poll.register(context.waker());
            }
        }
    }
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        if let Ok(peer) = self.peer() {
            peer.recv_poll.wake();
            peer.write_poll.wake();
        }
        self.inner.recv_poll.wake();
        self.inner.write_poll.wake();
        self.inner.accept_poll.wake();
        let local = self.inner.local_addr.lock().clone();
        if !matches!(local, UnixSocketAddr::Unnamed) {
            let mut binds = UNIX_BINDS.lock();
            if binds
                .get(&local)
                .is_some_and(|inner| Arc::ptr_eq(inner, &self.inner))
            {
                binds.remove(&local);
            }
        }
    }
}
