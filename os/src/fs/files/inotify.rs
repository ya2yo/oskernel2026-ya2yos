//! inotify 文件对象：文件系统事件监控。
//!
//! 参考 https://man7.org/linux/man-pages/man7/inotify.7.html
//!
//! 系统调用入口:
//! * [`crate::syscall::fs::sys_inotify_init1`]
//! * [`crate::syscall::fs::sys_inotify_add_watch`]
//! * [`crate::syscall::fs::sys_inotify_rm_watch`]
//!
//! 当前实现（第一步）：
//! * inotify fd 的 read/write/poll/fstat — 完整实现
//! * watch 管理（add/rm）— 完整实现
//! * 文件系统事件自动生成 — 待完成（第二步）
//!
//! 每个实例同时实现 [`File`] trait，因此可以像其他内核文件对象一样参与
//! fd 表查找、阻塞读和 poll。实例的全局注册表只保存 [`Weak`] 引用，系统调用
//! 通过 fd 查找到仍存活的实例后，再操作其 watch 表或事件队列；该注册表不是
//! 事件的来源，真正的事件生产接口是 [`InotifyFd::push_event`]。

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use spin::{Lazy, Mutex};

use crate::fs::vfs::File;
use crate::fs::Kstat;
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::task::{block_on, poll_io};
use crate::utils::{PollSet, SysErrNo, SysResult, SyscallRet};

// ---- inotify 事件掩码 ---------------------------------------------------

bitflags::bitflags! {
    /// inotify 事件掩码（来自 linux/inotify.h）
    pub struct InotifyMask: u32 {
        const ACCESS        = 0x00000001; // IN_ACCESS
        const MODIFY        = 0x00000002; // IN_MODIFY
        const ATTRIB        = 0x00000004; // IN_ATTRIB
        const CLOSE_WRITE   = 0x00000008; // IN_CLOSE_WRITE
        const CLOSE_NOWRITE = 0x00000010; // IN_CLOSE_NOWRITE
        const OPEN          = 0x00000020; // IN_OPEN
        const MOVED_FROM    = 0x00000040; // IN_MOVED_FROM
        const MOVED_TO      = 0x00000080; // IN_MOVED_TO
        const CREATE        = 0x00000100; // IN_CREATE
        const DELETE        = 0x00000200; // IN_DELETE
        const DELETE_SELF   = 0x00000400; // IN_DELETE_SELF
        const MOVE_SELF     = 0x00000800; // IN_MOVE_SELF

        // 辅助组合
        const CLOSE = Self::CLOSE_WRITE.bits() | Self::CLOSE_NOWRITE.bits();
        const MOVE  = Self::MOVED_FROM.bits() | Self::MOVED_TO.bits();

        // 特殊标志（用户可设置）
        const ONLYDIR       = 0x01000000; // IN_ONLYDIR
        const DONT_FOLLOW   = 0x02000000; // IN_DONT_FOLLOW
        const EXCL_UNLINK   = 0x04000000; // IN_EXCL_UNLINK
        const MASK_CREATE   = 0x10000000; // IN_MASK_CREATE
        const MASK_ADD      = 0x20000000; // IN_MASK_ADD
        const ONESHOT       = 0x80000000; // IN_ONESHOT
    }
}

/// 所有事件位的集合
const IN_ALL_EVENTS: u32 = InotifyMask::ACCESS.bits()
    | InotifyMask::MODIFY.bits()
    | InotifyMask::ATTRIB.bits()
    | InotifyMask::CLOSE_WRITE.bits()
    | InotifyMask::CLOSE_NOWRITE.bits()
    | InotifyMask::OPEN.bits()
    | InotifyMask::MOVED_FROM.bits()
    | InotifyMask::MOVED_TO.bits()
    | InotifyMask::CREATE.bits()
    | InotifyMask::DELETE.bits()
    | InotifyMask::DELETE_SELF.bits()
    | InotifyMask::MOVE_SELF.bits();

// ---- inotify_event 结构体 -------------------------------------------------

/// inotify_event — 用户态通过 read() 读取的事件结构。
///
/// 与 Linux 定义的布局一致：
/// ```c
/// struct inotify_event {
///     __s32 wd;      // watch descriptor
///     __u32 mask;    // event mask
///     __u32 cookie;  // rename 关联 cookie
///     __u32 len;     // name 长度（含 '\0'）
///     char name[];   // 可变长度文件名
/// };
/// ```
#[derive(Clone)]
pub struct InotifyEvent {
    /// 触发事件的 watch 描述符
    pub wd: i32,
    /// 事件掩码（实际发生的事件）
    pub mask: u32,
    /// rename 关联 cookie（非 rename 事件为 0）
    pub cookie: u32,
    /// 文件名（含 '\0' 结尾）
    pub name: Vec<u8>,
}

/// inotify_event 的固定头部大小（wd + mask + cookie + len）
const INOTIFY_EVENT_HEADER_SIZE: usize = size_of::<i32>() * 1 // wd
    + size_of::<u32>() * 3;
// mask + cookie + len

impl InotifyEvent {
    /// 序列化到字节缓冲区。
    ///
    /// 字段按 Linux `struct inotify_event` 的顺序以本机字节序写入；`name`
    /// 已由事件生产者负责准备（通常包含末尾的 `NUL`），本函数只负责原样
    /// 复制，不会修改或补齐名称内容。返回写入的字节数，若 buf 空间不足返回
    /// `None`。
    pub fn serialize_to(&self, buf: &mut [u8]) -> Option<usize> {
        let total = self.encoded_size();
        if buf.len() < total {
            return None;
        }

        let mut offset = 0;
        buf[offset..offset + 4].copy_from_slice(&self.wd.to_ne_bytes());
        offset += 4;
        buf[offset..offset + 4].copy_from_slice(&self.mask.to_ne_bytes());
        offset += 4;
        buf[offset..offset + 4].copy_from_slice(&self.cookie.to_ne_bytes());
        offset += 4;
        let name_len = self.name.len() as u32;
        buf[offset..offset + 4].copy_from_slice(&name_len.to_ne_bytes());
        offset += 4;
        buf[offset..offset + self.name.len()].copy_from_slice(&self.name);
        Some(total)
    }

    /// 返回编码后总字节数（固定头部加 `name` 字段）。
    ///
    /// 事件没有额外的对齐填充；因此调用者可以用该值判断一个完整事件是否
    /// 能放入用户提供的 read 缓冲区。
    pub fn encoded_size(&self) -> usize {
        INOTIFY_EVENT_HEADER_SIZE + self.name.len()
    }
}

// ---- 监视条目和 inotify fd ------------------------------------------------

/// 每个 `add_watch` 对应一个 `WatchEntry`。
///
/// 当前 watch 表按描述符保存路径和掩码；事件自动生成尚未接入，因此路径
/// 目前主要用于保留 watch 的内核状态，供后续文件系统事件匹配逻辑使用。
struct WatchEntry {
    /// 被监视的路径
    #[allow(dead_code)]
    path: String,
    /// 监视掩码
    mask: u32,
}

/// inotify 实例 — `inotify_init1` 创建，以 `FileClass::Abs` 存入 fd 表。
///
/// `watches` 负责保存用户注册的监视项，`event_queue` 是生产者与 `read(2)`
/// 消费者之间的 FIFO 队列。两者使用独立锁，入队只需持有队列锁，随后通过
/// `poll_rx` 唤醒可能阻塞的读者。
pub struct InotifyFd {
    /// 下一个可分配的 watch descriptor（从 1 开始自增）
    next_wd: AtomicI32,
    /// watch 列表：wd → WatchEntry
    watches: Mutex<BTreeMap<i32, WatchEntry>>,
    /// 事件队列（生产者：vfs 钩子；消费者：read）
    event_queue: Mutex<VecDeque<InotifyEvent>>,
    /// 读端唤醒器：新事件到来时唤醒阻塞的 reader
    poll_rx: PollSet,
    /// 是否非阻塞模式
    non_blocking: AtomicBool,
}

// ---- 全局注册表：按 fd 索引 InotifyFd（Arc<dyn File> 无法 downcast）-----

/// 全局 inotify 实例表，由 `sys_inotify_init1` / `add_watch` / `rm_watch` 使用。
static INOTIFY_TABLE: Lazy<Mutex<BTreeMap<usize, Weak<InotifyFd>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

impl InotifyFd {
    /// 创建一个新的 inotify 实例。
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            next_wd: AtomicI32::new(1),
            watches: Mutex::new(BTreeMap::new()),
            event_queue: Mutex::new(VecDeque::new()),
            poll_rx: PollSet::new(),
            non_blocking: AtomicBool::new(false),
        })
    }

    /// `sys_inotify_init1` 分配 fd 后将实例注册到全局表。
    pub fn register_fd(infd: usize, instance: &Arc<InotifyFd>) {
        INOTIFY_TABLE.lock().insert(infd, Arc::downgrade(instance));
    }

    /// 根据 fd 查找对应的 inotify 实例。
    pub fn lookup(infd: usize) -> Result<Arc<InotifyFd>, SysErrNo> {
        INOTIFY_TABLE
            .lock()
            .get(&infd)
            .and_then(|w| w.upgrade())
            .ok_or(SysErrNo::EBADF)
    }

    /// 清理已释放的 inotify 实例的弱引用。
    fn prune_dead_entries() {
        INOTIFY_TABLE.lock().retain(|_, w| w.upgrade().is_some());
    }

    /// 分配一个 watch descriptor 并存入监视列表。
    ///
    /// 描述符从 1 开始按实例递增分配；当前实现不复用已经删除的描述符，
    /// 也不在这里验证路径或解释标志位，相关参数语义由系统调用入口负责。
    pub fn add_watch(&self, path: String, mask: u32) -> SyscallRet {
        let wd = self.next_wd.fetch_add(1, Ordering::Relaxed);
        self.watches.lock().insert(wd, WatchEntry { path, mask });
        Ok(wd as usize)
    }

    /// 移除一个 watch descriptor。
    pub fn rm_watch(&self, wd: i32) -> SyscallRet {
        match self.watches.lock().remove(&wd) {
            Some(_) => Ok(0),
            None => Err(SysErrNo::EINVAL),
        }
    }

    /// 向事件队列推送一个事件并唤醒阻塞的 reader。
    #[allow(dead_code)] // 第二步启用
    pub fn push_event(&self, event: InotifyEvent) {
        self.event_queue.lock().push_back(event);
        self.poll_rx.wake();
    }

    /// 尝试从队列中取出最多 `buf.len()` 字节的完整事件并写入 `buf`。
    ///
    /// 事件不会被拆分：若当前事件无法放入但此前已经写入事件，则保留当前
    /// 事件供下次读取；若连一个事件也放不下则返回 `EINVAL`。返回值为写入
    /// 的字节数，队列为空时返回 `EAGAIN`。阻塞等待由 [`File::read`] 外层的
    /// [`poll_io`] 完成。

    fn try_read_events(&self, buf: &mut [u8]) -> SysResult<usize> {
        let mut queue = self.event_queue.lock();
        let mut written = 0usize;

        // 尽可能多地取出事件，直到 buf 满或队列空
        while let Some(event) = queue.front() {
            let total = event.encoded_size();
            if written + total > buf.len() && written > 0 {
                // 当前事件放不下但已有部分数据 → 下次再读此事件
                break;
            }
            if written + total > buf.len() {
                // buf 太小，连一个事件都放不下 → EINVAL
                return Err(SysErrNo::EINVAL);
            }
            if let Some(n) = event.serialize_to(&mut buf[written..]) {
                written += n;
                queue.pop_front();
            } else {
                // 理论上不应到达这里
                break;
            }
        }

        if written == 0 {
            Err(SysErrNo::EAGAIN)
        } else {
            Ok(written)
        }
    }
}

impl Drop for InotifyFd {
    fn drop(&mut self) {
        Self::prune_dead_entries();
    }
}

// ---- File trait ----------------------------------------------------------

impl File for InotifyFd {
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        false
    }

    /// 从 inotify 实例读取事件。
    ///
    /// * 阻塞模式：事件队列为空时挂起当前任务直到新事件到来。
    /// * 非阻塞模式：事件队列为空时立即返回 `EAGAIN`。
    /// * 返回读取的字节数。
    fn read(&self, mut dstbuf: UserBuffer) -> SyscallRet {
        if dstbuf.len() < INOTIFY_EVENT_HEADER_SIZE {
            return Err(SysErrNo::EINVAL);
        }

        let mut kernel_buf = vec![0u8; dstbuf.len()];
        let ret = block_on(poll_io(self, PollEvents::IN, self.nonblocking(), || {
            self.try_read_events(&mut kernel_buf)
        }))?;

        dstbuf.write(&kernel_buf[..ret]);
        Ok(ret)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn fstat(&self) -> Kstat {
        Kstat::default()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> SysResult {
        self.non_blocking.store(nonblocking, Ordering::Relaxed);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn poll(&self, _events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        let has_events = !self.event_queue.lock().is_empty();
        revents.set(PollEvents::IN, has_events);
        revents
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: PollEvents) {
        if events.contains(PollEvents::IN) {
            self.poll_rx.register(context.waker());
        }
    }
}
