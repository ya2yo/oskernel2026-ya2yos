use crate::arch::memory_layout::PAGE_SIZE;
#[cfg(feature = "perf")]
use crate::arch::time::get_ticks;
use crate::{
    fs::{
        fanotify_events_suppressed, notify_path_event, FilePageCacheSource, FsIndex, InodeType,
        Kstat, FAN_ACCESS, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_MODIFY, FILE_PAGE_CACHE,
        SEEK_CUR, SEEK_DATA, SEEK_END, SEEK_HOLE, SEEK_SET,
    },
    mm::{copy_from_user, copy_to_user, MemorySet, UserBuffer},
    syscall::PollEvents,
    utils::{SysErrNo, SyscallRet},
};

use super::{
    super::{File, Inode},
    pipe::set_pipe_max_size,
};
use alloc::{borrow::Cow, collections::BTreeMap, string::String, sync::Arc, vec};
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use linux_raw_sys::{
    general::FS_IMMUTABLE_FL,
    ioctl::{FS_IOC32_GETFLAGS, FS_IOC32_SETFLAGS, FS_IOC_GETFLAGS, FS_IOC_SETFLAGS},
};
use spin::{Lazy, Mutex};

static WRITE_OPEN_COUNTS: Lazy<Mutex<BTreeMap<String, usize>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));
/// Number of live open file descriptions for each VFS inode object.  This is
/// deliberately separate from `FSInfo`: a process may unlink a pathname that
/// is still open by a sibling process or retained by a file-backed mapping.
static OPEN_FILE_COUNTS: Lazy<Mutex<BTreeMap<usize, usize>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));
static FILE_FLAGS: Lazy<Mutex<BTreeMap<String, u32>>> = Lazy::new(|| Mutex::new(BTreeMap::new()));
static NEXT_OFD_LOCK_OWNER: AtomicI32 = AtomicI32::new(1);
const PIPE_MAX_SIZE_PATH: &str = "/proc/sys/fs/pipe-max-size";
const MAX_AGGREGATED_READ: usize = 64 * 1024;
/// Large compiler inputs can be read repeatedly by worker processes. The
/// shared cache is globally capped, so admit a moderate per-file size while
/// retaining a fixed upper bound on retained physical pages.
const MAX_PAGE_CACHED_READ_FILE_SIZE: usize = 32 * 1024 * 1024;

fn seek_offset(base: usize, offset: isize) -> Result<usize, SysErrNo> {
    if offset < 0 {
        let magnitude = offset.checked_neg().ok_or(SysErrNo::EINVAL)? as usize;
        base.checked_sub(magnitude).ok_or(SysErrNo::EINVAL)
    } else {
        base.checked_add(offset as usize).ok_or(SysErrNo::EINVAL)
    }
}

fn alloc_ofd_lock_owner() -> i32 {
    -NEXT_OFD_LOCK_OWNER.fetch_add(1, Ordering::Relaxed)
}

fn register_write_open(path: &str) {
    let mut counts = WRITE_OPEN_COUNTS.lock();
    *counts.entry(String::from(path)).or_insert(0) += 1;
}

fn unregister_write_open(path: &str) {
    let mut counts = WRITE_OPEN_COUNTS.lock();
    if let Some(count) = counts.get_mut(path) {
        *count -= 1;
        if *count == 0 {
            counts.remove(path);
        }
    }
}

fn register_open_inode(inode: &Arc<dyn Inode>) -> usize {
    // The canonical FsIndex inode Arc is shared by all opens of one live
    // object. Pointer identity also cannot confuse a newly reused ext4 inode
    // number with an older delayed-unlink object.
    let key = Arc::as_ptr(inode) as *const () as usize;
    let mut counts = OPEN_FILE_COUNTS.lock();
    *counts.entry(key).or_insert(0) += 1;
    key
}

fn unregister_open_inode(key: usize) {
    let mut counts = OPEN_FILE_COUNTS.lock();
    if let Some(count) = counts.get_mut(&key) {
        *count -= 1;
        if *count == 0 {
            counts.remove(&key);
        }
    }
}

fn has_other_open_inode(key: usize) -> bool {
    OPEN_FILE_COUNTS.lock().get(&key).copied().unwrap_or(0) > 1
}

fn get_file_flags(path: &str) -> u32 {
    FILE_FLAGS.lock().get(path).copied().unwrap_or(0)
}

fn set_file_flags(path: &str, flags: u32) {
    let mut attrs = FILE_FLAGS.lock();
    if flags == 0 {
        attrs.remove(path);
    } else {
        attrs.insert(String::from(path), flags);
    }
}

fn sync_pipe_max_size_sysctl(path: &str, bytes: &[u8]) -> Result<(), SysErrNo> {
    if path != PIPE_MAX_SIZE_PATH {
        return Ok(());
    }
    if bytes.is_empty() {
        return Ok(());
    }
    let end = bytes
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(bytes.len());
    if end == 0 {
        return Err(SysErrNo::EINVAL);
    }
    let mut value = 0usize;
    for &byte in &bytes[..end] {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add((byte - b'0') as usize))
            .ok_or(SysErrNo::EINVAL)?;
    }
    set_pipe_max_size(value)
}

fn is_immutable_path(path: &str) -> bool {
    get_file_flags(path) & FS_IMMUTABLE_FL != 0
}

/// “普通”的文件类
/// 区别于管道、设备、套接字等特殊文件
pub struct OSFile {
    readable: bool,     // 该文件是否允许通过 sys_read 进行读
    writable: bool,     // 该文件是否允许通过 sys_write 进行写
    append: AtomicBool, // O_APPEND: 每次 write 前都定位到文件末尾
    pub inode: Arc<dyn Inode>,
    /// 文件类型在 open() 时已经确定；lseek() 不应每次重新构造路径并查询
    /// 全局特殊节点表。对普通 inode 直接复用 inode.types()，对 FIFO/设备/socket
    /// 则保留创建时登记在 FsIndex 中的类型。
    seek_type: InodeType,
    /// Stable key used by [`OPEN_FILE_COUNTS`] for this open file description.
    open_inode_key: usize,
    write_path: Option<String>,
    suppress_fanotify: bool,
    ofd_lock_owner: i32,
    inner: Mutex<OSFileInner>,
}
struct OSFileInner {
    offset: usize, // 偏移量
}

impl OSFile {
    pub fn new(readable: bool, writable: bool, append: bool, inode: Arc<dyn Inode>) -> Self {
        let seek_type = Self::resolve_seek_type(&inode);
        let open_inode_key = register_open_inode(&inode);
        let write_path = if writable {
            let path = inode.path();
            register_write_open(&path);
            Some(path)
        } else {
            None
        };
        Self {
            readable,
            writable,
            append: AtomicBool::new(append),
            inode,
            seek_type,
            open_inode_key,
            write_path,
            suppress_fanotify: false,
            ofd_lock_owner: alloc_ofd_lock_owner(),
            inner: Mutex::new(OSFileInner { offset: 0 }),
        }
    }

    /// 创建 fanotify 事件中返回给用户态的目标 fd。
    ///
    /// 该 fd 的读写关闭不应再次生成 fanotify 事件，否则会污染原 notification
    /// group 的事件队列。
    pub fn new_fanotify_event(
        readable: bool,
        writable: bool,
        append: bool,
        inode: Arc<dyn Inode>,
    ) -> Self {
        let seek_type = Self::resolve_seek_type(&inode);
        let open_inode_key = register_open_inode(&inode);
        Self {
            readable,
            writable,
            append: AtomicBool::new(append),
            inode,
            seek_type,
            open_inode_key,
            write_path: None,
            suppress_fanotify: true,
            ofd_lock_owner: alloc_ofd_lock_owner(),
            inner: Mutex::new(OSFileInner { offset: 0 }),
        }
    }

    /// Resolve special-node type once while constructing the open file
    /// description.  Linux keeps the opened inode attached to `struct file`,
    /// so subsequent `lseek()` calls only inspect that stable object.  The
    /// fallback is important for normal EXT4 files and directories, whose
    /// type is already immutable in the VFS inode.
    fn resolve_seek_type(inode: &Arc<dyn Inode>) -> InodeType {
        let path = inode.path();
        FsIndex::special_node_type(&path).unwrap_or_else(|| inode.types())
    }

    pub fn is_write_open_path(path: &str) -> bool {
        WRITE_OPEN_COUNTS.lock().get(path).copied().unwrap_or(0) != 0
    }

    /// Whether a different open file description still refers to this inode.
    /// The current `OSFile` itself is included in the count, so unlink callers
    /// can distinguish their temporary lookup from a real live fd or mapping.
    pub fn has_other_open_reference(&self) -> bool {
        has_other_open_inode(self.open_inode_key)
    }

    pub fn is_immutable_path(path: &str) -> bool {
        is_immutable_path(path)
    }

    pub fn set_offset(&self, offset: usize) {
        self.inner.lock().offset = offset;
    }

    /// Directory streams store opaque cookies here too, so callers such as
    /// getdents64 must read it directly instead of routing through lseek().
    pub fn offset(&self) -> usize {
        self.inner.lock().offset
    }

    pub fn ofd_lock_owner(&self) -> i32 {
        self.ofd_lock_owner
    }

    /// Use the shared file-page cache for bounded regular-file reads.
    ///
    /// A cold single-page range loads one complete page, allowing later
    /// compiler workers to reuse it even when their `read(2)` buffer is only
    /// a few bytes.  Larger requests retain the aggregated-read fast path.
    /// Files that are too large or special still use the direct path.
    fn try_page_cached_read(
        &self,
        offset: usize,
        buf: &mut UserBuffer,
    ) -> Result<Option<usize>, SysErrNo> {
        let requested_len = buf.len();
        if requested_len > MAX_AGGREGATED_READ {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_file_cache_read_bypass_request_size(requested_len);
            return Ok(None);
        }
        if self.inode.types() != InodeType::File {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_file_cache_read_bypass_nonregular(requested_len);
            return Ok(None);
        }
        let file_size = self.inode.size();
        if file_size > MAX_PAGE_CACHED_READ_FILE_SIZE {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_file_cache_read_bypass_file_size(requested_len);
            return Ok(None);
        }
        if offset >= file_size {
            return Ok(Some(0));
        }

        if requested_len <= PAGE_SIZE {
            let page = FILE_PAGE_CACHE.get_or_load(
                self.inode.clone(),
                offset / PAGE_SIZE,
                FilePageCacheSource::Read,
            )?;
            let page_offset = offset % PAGE_SIZE;
            if page_offset >= page.valid_len {
                return Ok(Some(0));
            }
            let read_size = (page.valid_len - page_offset).min(requested_len);
            let page_bytes = page.frame.ppn.bytes_array();
            buf.write(&page_bytes[page_offset..page_offset + read_size]);
            return Ok(Some(read_size));
        }

        // A large read often straddles both resident and cold pages.  The old
        // all-or-nothing lookup treated one cold page as a miss for the whole
        // request, rereading every already cached page through lwext4.  That
        // inflated the serialized EXT4 read queue even when most of the
        // request was a page-cache hit.  Fill a temporary contiguous buffer by
        // copying resident pages and issuing I/O only for contiguous cold
        // runs; the resulting complete pages are published as before.
        let available = requested_len.min(file_size.saturating_sub(offset));
        let cache_path = self
            .inode
            .page_cache_path()
            .unwrap_or_else(|| Arc::from(self.inode.path().as_str()));
        let mut kernel_buf = vec![0; available];
        let mut cursor = 0usize;
        while cursor < available {
            let file_offset = offset.saturating_add(cursor);
            let page_index = file_offset / PAGE_SIZE;
            let page_offset = file_offset % PAGE_SIZE;
            if let Some(page) = FILE_PAGE_CACHE.get_shared(&cache_path, page_index) {
                #[cfg(feature = "perf")]
                crate::utils::perf::record_file_cache_read_hit(1);
                if page_offset >= page.valid_len {
                    break;
                }
                let read_size = (page.valid_len - page_offset).min(available - cursor);
                let page_bytes = page.frame.ppn.bytes_array();
                kernel_buf[cursor..cursor + read_size]
                    .copy_from_slice(&page_bytes[page_offset..page_offset + read_size]);
                cursor += read_size;
                continue;
            }

            let run_start = cursor;
            let first_page_end = page_index
                .saturating_add(1)
                .saturating_mul(PAGE_SIZE)
                .saturating_sub(offset)
                .min(available);
            let mut run_end = first_page_end.max(cursor.saturating_add(1));
            while run_end < available {
                let next_page_index = offset.saturating_add(run_end) / PAGE_SIZE;
                if FILE_PAGE_CACHE
                    .get_shared(&cache_path, next_page_index)
                    .is_some()
                {
                    break;
                }
                let next_page_end = next_page_index
                    .saturating_add(1)
                    .saturating_mul(PAGE_SIZE)
                    .saturating_sub(offset)
                    .min(available);
                if next_page_end <= run_end {
                    break;
                }
                run_end = next_page_end;
            }

            #[cfg(feature = "perf")]
            {
                let run_len = run_end - run_start;
                let cold_pages = (page_offset + run_len + PAGE_SIZE - 1) / PAGE_SIZE;
                crate::utils::perf::record_file_cache_read_miss(cold_pages);
            }

            let read_size = self.inode.read_at(
                offset.saturating_add(run_start),
                &mut kernel_buf[run_start..run_end],
            )?;
            #[cfg(feature = "perf")]
            crate::utils::perf::record_inode_read_source(
                crate::utils::perf::InodeReadSource::PageCachedReadColdRun,
                read_size,
            );
            if read_size != 0 {
                FILE_PAGE_CACHE.insert_read_range(
                    cache_path.as_ref(),
                    offset.saturating_add(run_start),
                    &kernel_buf[run_start..run_start + read_size],
                    file_size,
                );
                cursor = cursor.saturating_add(read_size);
            }
            if read_size < run_end - run_start {
                break;
            }
        }
        let read_size = cursor;
        if read_size != 0 {
            buf.write(&kernel_buf[..read_size]);
        }
        Ok(Some(read_size))
    }
}

impl Drop for OSFile {
    fn drop(&mut self) {
        if let Some(path) = self.write_path.as_deref() {
            unregister_write_open(path);
        }
        unregister_open_inode(self.open_inode_key);
        if !self.suppress_fanotify && !fanotify_events_suppressed() {
            let mask = if self.writable {
                FAN_CLOSE_WRITE
            } else {
                FAN_CLOSE_NOWRITE
            };
            notify_path_event(&self.inode.path(), mask);
        }
    }
}

// 为 OSFile 实现 File Trait
impl File for OSFile {
    fn readable(&self) -> bool {
        self.readable
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let mut inner = self.inner.lock();
        let mut total_read_size = 0usize;
        let requested_len = buf.len();

        if buf.buffers.is_empty() || requested_len == 0 {
            // EOF and zero-length reads are handled without entering the
            // filesystem adapter.  A non-empty read_at() returns 0 itself at
            // EOF, so a separate inode.size() probe would only add another
            // serialized EXT4 operation to every read syscall.
            return Ok(0);
        }

        if let Some(read_size) = self.try_page_cached_read(inner.offset, &mut buf)? {
            inner.offset += read_size;
            total_read_size = read_size;
        } else if buf.buffers.len() == 1 {
            // Keep the common single-page case zero-copy.
            let slice = &mut buf.buffers[0];
            let read_size = self.inode.read_at(inner.offset, slice)?;
            #[cfg(feature = "perf")]
            crate::utils::perf::record_inode_read_source(
                crate::utils::perf::InodeReadSource::DirectBypass,
                read_size,
            );
            inner.offset += read_size;
            total_read_size = read_size;
        } else if requested_len <= MAX_AGGREGATED_READ {
            // A user read commonly crosses a page boundary.  Entering the
            // EXT4 adapter once per page serializes the whole syscall on the
            // global lwext4 lock and repeats path/descriptor bookkeeping.  A
            // temporary contiguous buffer lets the filesystem perform one
            // read; the final copy only spans the user pages already supplied
            // by the syscall.
            let mut kernel_buf = vec![0; requested_len];
            let read_size = self.inode.read_at(inner.offset, &mut kernel_buf)?;
            #[cfg(feature = "perf")]
            crate::utils::perf::record_inode_read_source(
                crate::utils::perf::InodeReadSource::DirectBypass,
                read_size,
            );
            if read_size != 0 {
                buf.write(&kernel_buf[..read_size]);
                inner.offset += read_size;
                total_read_size = read_size;
            }
        } else {
            // Keep very large reads streaming so a user-controlled length
            // cannot force an unbounded temporary kernel allocation.
            for slice in buf.buffers.iter_mut() {
                let read_size = self.inode.read_at(inner.offset, slice)?;
                #[cfg(feature = "perf")]
                crate::utils::perf::record_inode_read_source(
                    crate::utils::perf::InodeReadSource::DirectBypass,
                    read_size,
                );
                if read_size == 0 {
                    break;
                }
                inner.offset += read_size;
                total_read_size += read_size;
            }
        }
        if total_read_size > 0 && !self.suppress_fanotify && !fanotify_events_suppressed() {
            notify_path_event(&self.inode.path(), FAN_ACCESS);
        }
        Ok(total_read_size)
    }

    fn write(&self, buf: UserBuffer) -> SyscallRet {
        let mut inner = self.inner.lock();
        let path = self.inode.path();
        if is_immutable_path(&path) {
            return Err(SysErrNo::EPERM);
        }
        if self.append.load(Ordering::Acquire) {
            inner.offset = self.inode.size();
        }
        let mut total_write_size = 0usize;
        for slice in buf.buffers.iter() {
            sync_pipe_max_size_sysctl(&path, slice)?;
            let write_offset = inner.offset;
            let write_size = self.inode.write_at(inner.offset, *slice)?;
            assert_eq!(write_size, slice.len());
            FILE_PAGE_CACHE.invalidate_path_range(&path, write_offset, write_size);
            inner.offset += write_size;
            total_write_size += write_size;
        }
        if total_write_size > 0 && !self.suppress_fanotify && !fanotify_events_suppressed() {
            notify_path_event(&path, FAN_MODIFY);
        }
        Ok(total_write_size)
    }

    fn fstat(&self) -> Kstat {
        self.inode.fstat()
    }

    fn path(&self) -> Cow<'_, str> {
        Cow::Owned(self.inode.path())
    }

    fn set_append(&self, append: bool) -> Result<(), SysErrNo> {
        self.append.store(append, Ordering::Release);
        Ok(())
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) && self.readable {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) && self.writable {
            revents |= PollEvents::OUT;
        }
        revents
    }
    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet {
        #[cfg(feature = "perf")]
        let _lseek_guard = crate::utils::perf::LseekDurationGuard::new();

        if self.seek_type.is_fifo() || self.seek_type.is_socket() {
            return Err(SysErrNo::ESPIPE);
        }
        let mut inner = self.inner.lock();
        let new_offset = match whence {
            SEEK_SET => {
                if offset < 0 {
                    return Err(SysErrNo::EINVAL);
                }
                offset as usize
            }
            SEEK_CUR => seek_offset(inner.offset, offset)?,
            SEEK_END => {
                #[cfg(feature = "perf")]
                let size_begin = get_ticks();
                let size = self.inode.size();
                #[cfg(feature = "perf")]
                crate::utils::perf::record_lseek_size_duration(
                    get_ticks().saturating_sub(size_begin),
                );
                seek_offset(size, offset)?
            }
            SEEK_DATA => {
                if offset < 0 {
                    return Err(SysErrNo::ENXIO);
                }
                #[cfg(feature = "perf")]
                let sparse_begin = get_ticks();
                let data = self.inode.seek_data(offset as usize);
                #[cfg(feature = "perf")]
                crate::utils::perf::record_lseek_sparse_duration(
                    get_ticks().saturating_sub(sparse_begin),
                );
                data?
            }
            SEEK_HOLE => {
                if offset < 0 {
                    return Err(SysErrNo::ENXIO);
                }
                #[cfg(feature = "perf")]
                let sparse_begin = get_ticks();
                let hole = self.inode.seek_hole(offset as usize);
                #[cfg(feature = "perf")]
                crate::utils::perf::record_lseek_sparse_duration(
                    get_ticks().saturating_sub(sparse_begin),
                );
                hole?
            }
            _ => return Err(SysErrNo::EINVAL),
        };
        // `off_t` is signed in the Linux ABI.  Do not let a valid `usize`
        // calculation cross into the negative half of the user-visible
        // return register.
        if new_offset > isize::MAX as usize {
            return Err(SysErrNo::EOVERFLOW);
        }
        inner.offset = new_offset;
        Ok(new_offset)
    }

    fn ioctl(&self, cmd: u32, arg: usize, memory_set: &MemorySet) -> SyscallRet {
        match cmd {
            FS_IOC_GETFLAGS | FS_IOC32_GETFLAGS => {
                let flags = get_file_flags(&self.inode.path());
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(
                        &flags as *const u32 as *const u8,
                        core::mem::size_of::<u32>(),
                    )
                })?;
                Ok(0)
            }
            FS_IOC_SETFLAGS | FS_IOC32_SETFLAGS => {
                let mut flags: u32 = 0;
                copy_from_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts_mut(
                        &mut flags as *mut u32 as *mut u8,
                        core::mem::size_of::<u32>(),
                    )
                })?;
                set_file_flags(&self.inode.path(), flags);
                Ok(0)
            }
            _ => Err(SysErrNo::ENOTTY),
        }
    }
}
