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
    mm::UserBuffer,
    net::{
        options::{Configurable, GetSocketOption, SetSocketOption, UnixCredentials},
        RecvFlags, RecvOptions, SendOptions, Shutdown, Socket, SocketAddrEx, SocketOps,
    },
    syscall::PollEvents,
    task::current_task,
    utils::{SysErrNo, SysResult},
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
}

struct UnixMessage {
    data: Vec<u8>,
    sender: UnixSocketAddr,
}

struct UnixSocketInner {
    kind: UnixSocketKind,
    local_addr: Mutex<UnixSocketAddr>,
    peer_addr: Mutex<UnixSocketAddr>,
    peer: Mutex<Option<Weak<UnixSocketInner>>>,
    recv_queue: Mutex<VecDeque<UnixMessage>>,
    pending: Mutex<VecDeque<Arc<UnixSocketInner>>>,
    listening: Mutex<bool>,
    nonblocking: Mutex<bool>,
    recv_closed: Mutex<bool>,
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
            recv_queue: Mutex::new(VecDeque::new()),
            pending: Mutex::new(VecDeque::new()),
            listening: Mutex::new(false),
            nonblocking: Mutex::new(false),
            recv_closed: Mutex::new(false),
            send_closed: Mutex::new(false),
            pid,
        }
    }
}

static UNIX_BINDS: Lazy<Mutex<HashMap<UnixSocketAddr, Arc<UnixSocketInner>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

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

    pub fn new_stream_pair() -> (Self, Self) {
        Self::new_pair(UnixSocketKind::Stream)
    }

    pub fn new_dgram_pair() -> (Self, Self) {
        Self::new_pair(UnixSocketKind::Dgram)
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
            UnixSocketKind::Stream => {
                if !*remote.listening.lock() {
                    return Err(SysErrNo::ECONNREFUSED);
                }
                let server = Arc::new(UnixSocketInner::new(UnixSocketKind::Stream, remote.pid));
                let local = self.inner.local_addr.lock().clone();
                *server.local_addr.lock() = remote_addr.clone();
                *server.peer_addr.lock() = local.clone();
                *server.peer.lock() = Some(Arc::downgrade(&self.inner));
                *self.inner.peer.lock() = Some(Arc::downgrade(&server));
                *self.inner.peer_addr.lock() = remote_addr;
                remote.pending.lock().push_back(server);
            }
            UnixSocketKind::Dgram => {
                *self.inner.peer.lock() = Some(Arc::downgrade(&remote));
                *self.inner.peer_addr.lock() = remote_addr;
            }
        }
        Ok(())
    }

    fn listen(&self) -> SysResult {
        if self.kind() != UnixSocketKind::Stream {
            return Err(SysErrNo::EOPNOTSUPP);
        }
        if matches!(*self.inner.local_addr.lock(), UnixSocketAddr::Unnamed) {
            return Err(SysErrNo::EINVAL);
        }
        *self.inner.listening.lock() = true;
        Ok(())
    }

    fn accept(&self) -> SysResult<Socket> {
        if self.kind() != UnixSocketKind::Stream || !*self.inner.listening.lock() {
            return Err(SysErrNo::EINVAL);
        }
        let accepted = self
            .inner
            .pending
            .lock()
            .pop_front()
            .ok_or(SysErrNo::EAGAIN)?;
        Ok(Socket::Unix(UnixSocket { inner: accepted }))
    }

    fn send(&self, mut src: UserBuffer, options: SendOptions) -> SysResult<usize> {
        if *self.inner.send_closed.lock() {
            return Err(SysErrNo::EPIPE);
        }
        let len = src.len();
        let data = src.read(len);
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
        if *target.recv_closed.lock() {
            return Err(SysErrNo::EPIPE);
        }
        let sender = self.inner.local_addr.lock().clone();
        target.recv_queue.lock().push_back(UnixMessage { data, sender });
        Ok(len)
    }

    fn recv(&self, mut dst: UserBuffer, options: RecvOptions<'_>) -> SysResult<usize> {
        if *self.inner.recv_closed.lock() {
            return Ok(0);
        }
        let mut queue = self.inner.recv_queue.lock();
        let mut message = queue.pop_front().ok_or(SysErrNo::EAGAIN)?;
        let written = dst.write(&message.data);
        if self.kind() == UnixSocketKind::Stream && written < message.data.len() {
            message.data = message.data[written..].to_vec();
            queue.push_front(message);
        } else {
            if let Some(from) = options.from {
                *from = SocketAddrEx::Unix(message.sender);
            }
            if options.flags.contains(RecvFlags::TRUNCATE) {
                return Ok(message.data.len());
            }
        }
        Ok(written)
    }

    fn local_addr(&self) -> SysResult<SocketAddrEx> {
        Ok(SocketAddrEx::Unix(self.inner.local_addr.lock().clone()))
    }

    fn peer_addr(&self) -> SysResult<SocketAddrEx> {
        let peer = self.inner.peer_addr.lock().clone();
        if matches!(peer, UnixSocketAddr::Unnamed) {
            Err(SysErrNo::ENOTCONN)
        } else {
            Ok(SocketAddrEx::Unix(peer))
        }
    }

    fn shutdown(&self, how: Shutdown) -> SysResult {
        if how.has_read() {
            *self.inner.recv_closed.lock() = true;
        }
        if how.has_write() {
            *self.inner.send_closed.lock() = true;
        }
        Ok(())
    }
}

impl crate::fs::File for UnixSocket {
    fn poll(&self, _events: PollEvents) -> PollEvents {
        let mut events = PollEvents::OUT | PollEvents::WRNORM;
        if !self.inner.recv_queue.lock().is_empty() || !self.inner.pending.lock().is_empty() {
            events |= PollEvents::IN | PollEvents::RDNORM;
        }
        events
    }

    fn register(&self, _context: &mut Context<'_>, _events: PollEvents) {}
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        let local = self.inner.local_addr.lock().clone();
        if !matches!(local, UnixSocketAddr::Unnamed) {
            let mut binds = UNIX_BINDS.lock();
            if binds.get(&local).is_some_and(|inner| Arc::ptr_eq(inner, &self.inner)) {
                binds.remove(&local);
            }
        }
    }
}
