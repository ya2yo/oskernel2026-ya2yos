//! fanotify 实例文件对象。
//!
//! 当前实现 `fanotify_init(2)` 创建出的 fd 载体、`fanotify_mark(2)` 的 mark
//! 表管理，以及覆盖 LTP `fanotify01` 所需的基础事件队列。权限事件响应、FID
//! 附加信息和完整 mount/filesystem 传播语义仍需要后续接入。

use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use spin::{Lazy, Mutex};

use crate::{
    fs::{open, vfs::File, FileClass, FileDescriptor, Kstat, OSFile, OpenFlags, NONE_MODE},
    mm::UserBuffer,
    syscall::PollEvents,
    task::{block_on, current_task, poll_io},
    utils::{PollSet, SysErrNo, SysResult, SyscallRet},
};

/// fanotify event metadata 版本。
const FANOTIFY_METADATA_VERSION: u8 = 3;
/// `struct fanotify_event_metadata` 的固定长度。
const FANOTIFY_EVENT_METADATA_LEN: usize = size_of::<u32>()
    + size_of::<u8>() * 2
    + size_of::<u16>()
    + size_of::<u64>()
    + size_of::<i32>() * 2;
/// `fanotify_init()` 的 file-handle reporting 标志。
const FAN_REPORT_FID: u32 = 0x0000_0200;
const FAN_REPORT_DIR_FID: u32 = 0x0000_0400;
const FAN_REPORT_NAME: u32 = 0x0000_0800;
const FAN_REPORT_TARGET_FID: u32 = 0x0000_1000;
/// legacy metadata 中没有可返回 fd 时使用的值。
const FAN_NOFD: i32 = -1;

/// 文件被访问。
pub const FAN_ACCESS: u64 = 0x0000_0001;
/// 文件被修改。
pub const FAN_MODIFY: u64 = 0x0000_0002;
/// 可写 fd 被关闭。
pub const FAN_CLOSE_WRITE: u64 = 0x0000_0008;
/// 只读 fd 被关闭。
pub const FAN_CLOSE_NOWRITE: u64 = 0x0000_0010;
/// 文件被打开。
pub const FAN_OPEN: u64 = 0x0000_0020;

/// 单个 fanotify mark 的内核侧记录。
struct FanotifyMark {
    /// 普通事件 mask。
    mask: u64,
    /// ignore mask，用于 `FAN_MARK_IGNORED_MASK` / `FAN_MARK_IGNORE`。
    ignored_mask: u64,
    /// 不会被 modify 事件清除的 ignore mask 位。
    ignored_surv_mask: u64,
}

/// 排队给用户态读取的 fanotify 事件。
#[derive(Clone)]
struct FanotifyEvent {
    /// 事件 mask。
    mask: u64,
    /// 触发事件的进程 ID。
    pid: i32,
    /// 事件目标路径，用于 legacy metadata 中返回可读 fd。
    path: String,
}

/// 一个 fanotify notification group。
pub struct FanotifyFd {
    init_flags: u32,
    event_f_flags: u32,
    nonblocking: AtomicBool,
    /// mark 表：`(mark_type, absolute_path)` → mark 状态。
    marks: Mutex<BTreeMap<(u32, String), FanotifyMark>>,
    /// 待 read() 取走的事件队列。
    event_queue: Mutex<VecDeque<FanotifyEvent>>,
    /// 读端唤醒器。
    poll_rx: PollSet,
}

/// 全局 fanotify 实例表，由 `fanotify_init` / `fanotify_mark` / 文件事件 hook 使用。
static FANOTIFY_TABLE: Lazy<Mutex<BTreeMap<usize, Weak<FanotifyFd>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));
static FANOTIFY_SUPPRESS_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// fanotify 内部操作的事件抑制 guard。
pub struct FanotifySuppressGuard;

impl Drop for FanotifySuppressGuard {
    fn drop(&mut self) {
        FANOTIFY_SUPPRESS_DEPTH.fetch_sub(1, Ordering::Release);
    }
}

/// 暂时抑制 fanotify 内部操作产生的文件事件。
pub fn suppress_fanotify_events() -> FanotifySuppressGuard {
    FANOTIFY_SUPPRESS_DEPTH.fetch_add(1, Ordering::AcqRel);
    FanotifySuppressGuard
}

/// 当前是否处于 fanotify 内部事件抑制区间。
pub fn fanotify_events_suppressed() -> bool {
    FANOTIFY_SUPPRESS_DEPTH.load(Ordering::Acquire) != 0
}

impl FanotifyFd {
    /// 创建 fanotify 实例。
    pub fn new(init_flags: u32, event_f_flags: u32, nonblocking: bool) -> Arc<Self> {
        Arc::new(Self {
            init_flags,
            event_f_flags,
            nonblocking: AtomicBool::new(nonblocking),
            marks: Mutex::new(BTreeMap::new()),
            event_queue: Mutex::new(VecDeque::new()),
            poll_rx: PollSet::new(),
        })
    }

    /// `fanotify_init()` 分配 fd 后将实例注册到全局表。
    pub fn register_fd(fd: usize, instance: &Arc<FanotifyFd>) {
        FANOTIFY_TABLE.lock().insert(fd, Arc::downgrade(instance));
    }

    /// 根据 fd 查找对应 fanotify notification group。
    pub fn lookup(fd: usize) -> Result<Arc<FanotifyFd>, SysErrNo> {
        FANOTIFY_TABLE
            .lock()
            .get(&fd)
            .and_then(|w| w.upgrade())
            .ok_or(SysErrNo::EBADF)
    }

    /// 清理已经释放实例的弱引用。
    fn prune_dead_entries() {
        FANOTIFY_TABLE.lock().retain(|_, w| w.upgrade().is_some());
    }

    /// 创建时传给 `fanotify_init()` 的 fanotify flags。
    pub fn init_flags(&self) -> u32 {
        self.init_flags
    }

    /// 后续事件 fd 使用的 open flags。
    pub fn event_f_flags(&self) -> u32 {
        self.event_f_flags
    }

    /// 添加或合并一个 fanotify mark。
    pub fn add_mark(
        &self,
        mark_type: u32,
        path: String,
        mask: u64,
        ignored: bool,
        ignored_survive: bool,
    ) -> SyscallRet {
        let mut marks = self.marks.lock();
        let entry = marks.entry((mark_type, path)).or_insert(FanotifyMark {
            mask: 0,
            ignored_mask: 0,
            ignored_surv_mask: 0,
        });
        if ignored {
            entry.ignored_mask |= mask;
            if ignored_survive {
                entry.ignored_surv_mask |= mask;
            } else {
                entry.ignored_surv_mask &= !mask;
            }
        } else {
            entry.mask |= mask;
        }
        Ok(0)
    }

    /// 从现有 mark 中移除指定 mask。
    pub fn remove_mark(
        &self,
        mark_type: u32,
        path: String,
        mask: u64,
        ignored: bool,
    ) -> SyscallRet {
        let key = (mark_type, path);
        let mut marks = self.marks.lock();
        let Some(entry) = marks.get_mut(&key) else {
            return Err(SysErrNo::ENOENT);
        };

        let target_mask = if ignored {
            &mut entry.ignored_mask
        } else {
            &mut entry.mask
        };
        if *target_mask & mask == 0 {
            return Err(SysErrNo::ENOENT);
        }
        *target_mask &= !mask;
        if ignored {
            entry.ignored_surv_mask &= !mask;
        }

        if entry.mask == 0 && entry.ignored_mask == 0 {
            marks.remove(&key);
        }
        Ok(0)
    }

    /// 清空指定 mark 类型下的所有 mark。
    pub fn flush_marks(&self, mark_type: u32) -> SyscallRet {
        self.marks.lock().retain(|(ty, _), _| *ty != mark_type);
        Ok(0)
    }

    fn reports_file_handle(&self) -> bool {
        self.init_flags
            & (FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME | FAN_REPORT_TARGET_FID)
            != 0
    }

    fn allocate_event_fd(&self, path: &str) -> i32 {
        let flags = OpenFlags::from_bits_truncate(self.event_f_flags);
        let suppress = suppress_fanotify_events();
        let Ok(file_class) = open(path, flags, NONE_MODE) else {
            return FAN_NOFD;
        };
        let FileClass::File(file) = file_class else {
            return FAN_NOFD;
        };
        let readable = file.readable();
        let writable = file.writable();
        let inode = file.inode.clone();
        drop(file);
        drop(suppress);

        let Some(task) = current_task() else {
            return FAN_NOFD;
        };
        let proc_inner = &task.process;
        let Ok(fd) = proc_inner.fd_table.alloc_fd() else {
            return FAN_NOFD;
        };

        let event_file = OSFile::new_fanotify_event(readable, writable, false, inode);
        if proc_inner
            .fd_table
            .set(
                fd,
                FileDescriptor::new(flags, FileClass::File(Arc::new(event_file))),
            )
            .is_err()
        {
            return FAN_NOFD;
        }
        fd as i32
    }

    fn serialize_event(&self, event: &FanotifyEvent, buf: &mut [u8]) -> Option<usize> {
        if buf.len() < FANOTIFY_EVENT_METADATA_LEN {
            return None;
        }

        let fd = if self.reports_file_handle() {
            FAN_NOFD
        } else {
            self.allocate_event_fd(&event.path)
        };

        let event_len = FANOTIFY_EVENT_METADATA_LEN as u32;
        let metadata_len = FANOTIFY_EVENT_METADATA_LEN as u16;
        let mut offset = 0usize;
        buf[offset..offset + 4].copy_from_slice(&event_len.to_ne_bytes());
        offset += 4;
        buf[offset] = FANOTIFY_METADATA_VERSION;
        offset += 1;
        buf[offset] = 0;
        offset += 1;
        buf[offset..offset + 2].copy_from_slice(&metadata_len.to_ne_bytes());
        offset += 2;
        buf[offset..offset + 8].copy_from_slice(&event.mask.to_ne_bytes());
        offset += 8;
        buf[offset..offset + 4].copy_from_slice(&fd.to_ne_bytes());
        offset += 4;
        buf[offset..offset + 4].copy_from_slice(&event.pid.to_ne_bytes());
        Some(FANOTIFY_EVENT_METADATA_LEN)
    }

    fn try_read_events(&self, buf: &mut [u8]) -> SysResult<usize> {
        if buf.len() < FANOTIFY_EVENT_METADATA_LEN {
            return Err(SysErrNo::EINVAL);
        }

        let mut events = Vec::new();
        {
            let mut queue = self.event_queue.lock();
            while !queue.is_empty() && (events.len() + 1) * FANOTIFY_EVENT_METADATA_LEN <= buf.len()
            {
                events.push(queue.pop_front().unwrap());
            }
        }

        if events.is_empty() {
            return Err(SysErrNo::EAGAIN);
        }

        let mut written = 0usize;
        for event in events.iter() {
            self.serialize_event(event, &mut buf[written..])
                .ok_or(SysErrNo::EINVAL)?;
            written += FANOTIFY_EVENT_METADATA_LEN;
        }
        Ok(written)
    }

    fn push_if_marked(&self, path: &str, mask: u64, pid: i32) {
        let mut should_push = false;
        {
            let mut marks = self.marks.lock();
            for ((_, marked_path), mark) in marks.iter_mut() {
                if marked_path != path {
                    continue;
                }
                if mask == FAN_MODIFY {
                    mark.ignored_mask &= mark.ignored_surv_mask;
                }
                if mark.mask & mask == 0 {
                    continue;
                }
                if mark.ignored_mask & mask != 0 {
                    continue;
                }
                should_push = true;
                break;
            }
        }

        if should_push {
            self.event_queue.lock().push_back(FanotifyEvent {
                mask,
                pid,
                path: String::from(path),
            });
            self.poll_rx.wake();
        }
    }
}

/// 向所有匹配 mark 的 fanotify group 投递路径事件。
pub fn notify_path_event(path: &str, mask: u64) {
    let Some(task) = current_task() else {
        return;
    };
    let pid = task.pid() as i32;
    let groups: Vec<Arc<FanotifyFd>> = FANOTIFY_TABLE
        .lock()
        .values()
        .filter_map(Weak::upgrade)
        .collect();
    for group in groups {
        group.push_if_marked(path, mask, pid);
    }
}

impl Drop for FanotifyFd {
    fn drop(&mut self) {
        Self::prune_dead_entries();
    }
}

impl File for FanotifyFd {
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        false
    }

    fn read(&self, mut dstbuf: UserBuffer) -> SyscallRet {
        if dstbuf.len() < FANOTIFY_EVENT_METADATA_LEN {
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

    fn nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, nonblocking: bool) -> SysResult {
        self.nonblocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    fn poll(&self, _events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        revents.set(PollEvents::IN, !self.event_queue.lock().is_empty());
        revents
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: PollEvents) {
        if events.contains(PollEvents::IN) {
            self.poll_rx.register(context.waker());
        }
    }
}
