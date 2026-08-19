//! `O_TMPFILE` 与 `memfd_create(2)` 使用的内存匿名普通文件。
//!
//! 文件数据、当前偏移、权限和 seal 状态全部保存在内核内存中，不创建 ext4
//! 目录项；只有通过后续 `linkat` 物化时才可能获得路径。`memfd` 还共享 seal
//! 与 mmap 页状态，使 `F_SEAL_*` 能约束写入、扩展、收缩及可写共享映射。
//!
//! [`TmpFile`] 实现 [`File`] 的读写、truncate、fallocate、seek、poll、append、
//! seal 和 mmap backing 行为；普通 O_TMPFILE 与 memfd 的差异由 `is_memfd` 控制。
//!
//! Linux `O_TMPFILE` opens a regular file that is not linked into any
//! directory yet. The directory path passed to `openat` only selects the
//! filesystem/context; the returned fd owns the file until it is closed or
//! later materialized with `linkat("/proc/self/fd/<fd>", ...)`. This object
//! therefore stores data, offset, mode and owner in memory, and intentionally
//! does not create an ext4 directory entry.
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use spin::Mutex;

use super::super::{File, Kstat, MmapBacking, StMode};
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SysResult, SyscallRet};
use crate::{
    arch::memory_layout::PAGE_SIZE,
    mm::{FrameTracker, UserBuffer},
};

static NEXT_TMP_INO: AtomicUsize = AtomicUsize::new(0x7000_0000);

const F_SEAL_SEAL: u32 = linux_raw_sys::general::F_SEAL_SEAL;
const F_SEAL_SHRINK: u32 = linux_raw_sys::general::F_SEAL_SHRINK;
const F_SEAL_GROW: u32 = linux_raw_sys::general::F_SEAL_GROW;
const F_SEAL_WRITE: u32 = linux_raw_sys::general::F_SEAL_WRITE;
const F_SEAL_FUTURE_WRITE: u32 = linux_raw_sys::general::F_SEAL_FUTURE_WRITE;
const F_SEAL_EXEC: u32 = linux_raw_sys::general::F_SEAL_EXEC;
const WRITE_SEALS: u32 = F_SEAL_WRITE | F_SEAL_FUTURE_WRITE;
const FALLOC_FL_KEEP_SIZE: u32 = 0x01;
const FALLOC_FL_PUNCH_HOLE: u32 = 0x02;
const SUPPORTED_SEALS: u32 =
    F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_FUTURE_WRITE | F_SEAL_EXEC;

/// Anonymous regular file returned by `openat(..., O_TMPFILE, ...)`.
///
/// It deliberately implements only fd-visible state. A real filesystem inode is
/// created later only if userspace links `/proc/self/fd/<fd>` into a directory.
pub struct TmpFile {
    /// 当前打开描述符是否允许 read(2)。
    readable: bool,
    /// 当前打开描述符是否允许 write(2)。
    writable: bool,
    /// fstat 返回的权限位。
    mode: u32,
    /// 文件所有者 uid/gid。
    uid: u32,
    gid: u32,
    /// 匿名 inode 编号。
    ino: usize,
    /// O_APPEND 状态；每次写入前将偏移移到文件末尾。
    append: AtomicBool,
    /// 是否为 memfd；仅 memfd 支持 seal、fallocate 和 mmap backing。
    is_memfd: bool,
    /// 创建时是否允许添加 seal。
    allow_sealing: bool,
    /// 与 reopen/mmap 共享的 seal 位图。
    seals: Arc<AtomicU32>,
    /// 共享映射及其页缓存状态。
    mmap_state: Arc<TmpFileMmapState>,
    /// 文件数据，供读写和 mmap 回写共同访问。
    inner: Arc<Mutex<TmpFileInner>>,
    /// 本 open file description 的独立文件偏移。
    offset: Mutex<usize>,
}

/// 匿名文件的实际字节内容。
///
/// 访问该向量必须持有外层互斥锁；共享 mmap 的脏页会在读写、截断或
/// 分配操作前同步回这里，从而让文件描述符操作观察到映射产生的修改。
struct TmpFileInner {
    data: Vec<u8>,
}

struct TmpFileMmapState {
    writable_shared_mappings: Mutex<usize>,
    shared_pages: Mutex<BTreeMap<usize, Weak<FrameTracker>>>,
}

struct TmpFileMmapBacking {
    state: Arc<TmpFileMmapState>,
    seals: Arc<AtomicU32>,
    inner: Arc<Mutex<TmpFileInner>>,
    shared: bool,
    writable_shared: bool,
}

impl Drop for TmpFileMmapBacking {
    /// 释放可写共享映射登记，使后续 `F_SEAL_WRITE` 可以成功安装。
    fn drop(&mut self) {
        if self.writable_shared {
            let mut mappings = self.state.writable_shared_mappings.lock();
            debug_assert!(*mappings > 0);
            *mappings -= 1;
        }
    }
}

impl TmpFileMmapBacking {
    /// 返回页在文件中的有效范围；文件末页可能短于一个完整页。
    fn page_bounds(data_len: usize, page_index: usize) -> Option<(usize, usize)> {
        let start = page_index.checked_mul(PAGE_SIZE)?;
        (start < data_len).then(|| (start, (data_len - start).min(PAGE_SIZE)))
    }

    /// 将仍存活的共享映射页写回文件内容，并清理已经释放的页引用。
    fn flush_shared_pages(
        pages: &mut BTreeMap<usize, Weak<FrameTracker>>,
        inner: &mut TmpFileInner,
    ) {
        let stale: Vec<usize> = pages
            .iter()
            .filter_map(|(page_index, frame)| frame.upgrade().is_none().then_some(*page_index))
            .collect();
        for page_index in stale {
            pages.remove(&page_index);
        }
        for (page_index, frame) in pages.iter() {
            let Some(frame) = frame.upgrade() else {
                continue;
            };
            let Some((start, valid_len)) = Self::page_bounds(inner.data.len(), *page_index) else {
                continue;
            };
            inner.data[start..start + valid_len]
                .copy_from_slice(&frame.ppn.bytes_array()[..valid_len]);
        }
    }

    /// 用最新文件内容刷新已建立的共享映射页；缩短文件时会清零页尾。
    fn refresh_shared_pages(pages: &mut BTreeMap<usize, Weak<FrameTracker>>, inner: &TmpFileInner) {
        let stale: Vec<usize> = pages
            .iter()
            .filter_map(|(page_index, frame)| frame.upgrade().is_none().then_some(*page_index))
            .collect();
        for page_index in stale {
            pages.remove(&page_index);
        }
        for (page_index, frame) in pages.iter() {
            let Some(frame) = frame.upgrade() else {
                continue;
            };
            let bytes = frame.ppn.bytes_array_mut();
            bytes.fill(0);
            let Some((start, valid_len)) = Self::page_bounds(inner.data.len(), *page_index) else {
                continue;
            };
            bytes[..valid_len].copy_from_slice(&inner.data[start..start + valid_len]);
        }
    }
}

impl MmapBacking for TmpFileMmapBacking {
    /// 共享映射只有在对应写入 seal 未安装时才允许写入。
    fn allows_write(&self) -> bool {
        !self.shared
            || self.seals.load(Ordering::Acquire) & (F_SEAL_WRITE | F_SEAL_FUTURE_WRITE) == 0
    }

    /// 获取或创建共享映射页，并以当前文件内容初始化新页。
    fn shared_page(&self, page_index: usize) -> Result<Option<Arc<FrameTracker>>, SysErrNo> {
        if !self.shared {
            return Ok(None);
        }
        let mut pages = self.state.shared_pages.lock();
        if let Some(frame) = pages.get(&page_index).and_then(Weak::upgrade) {
            return Ok(Some(frame));
        }
        let inner = self.inner.lock();
        let Some((start, valid_len)) = Self::page_bounds(inner.data.len(), page_index) else {
            return Ok(None);
        };
        let frame = FrameTracker::alloc().ok_or(SysErrNo::ENOMEM)?;
        frame.ppn.bytes_array_mut()[..valid_len]
            .copy_from_slice(&inner.data[start..start + valid_len]);
        pages.insert(page_index, Arc::downgrade(&frame));
        Ok(Some(frame))
    }

    /// 将文件指定页加载到调用方提供的物理帧中，并返回有效字节数。
    fn load_page(
        &self,
        page_index: usize,
        frame: &FrameTracker,
    ) -> Result<Option<usize>, SysErrNo> {
        let mut pages = self.state.shared_pages.lock();
        let mut inner = self.inner.lock();
        Self::flush_shared_pages(&mut pages, &mut inner);
        let Some((start, valid_len)) = Self::page_bounds(inner.data.len(), page_index) else {
            return Ok(None);
        };
        frame.ppn.bytes_array_mut()[..valid_len]
            .copy_from_slice(&inner.data[start..start + valid_len]);
        Ok(Some(valid_len))
    }

    fn page_valid_len(&self, page_index: usize) -> Option<usize> {
        let inner = self.inner.lock();
        Self::page_bounds(inner.data.len(), page_index).map(|(_, valid_len)| valid_len)
    }

    /// 将共享映射的脏页写回匿名文件；私有映射不产生文件回写。
    fn writeback_page(&self, page_index: usize, frame: &FrameTracker) -> SysResult {
        if !self.shared {
            return Ok(());
        }
        let _pages = self.state.shared_pages.lock();
        let mut inner = self.inner.lock();
        let Some((start, valid_len)) = Self::page_bounds(inner.data.len(), page_index) else {
            return Ok(());
        };
        inner.data[start..start + valid_len].copy_from_slice(&frame.ppn.bytes_array()[..valid_len]);
        Ok(())
    }
}

impl TmpFile {
    /// 创建不支持 seals 的匿名 `O_TMPFILE` 文件。
    pub fn new(readable: bool, writable: bool, mode: u32, uid: u32, gid: u32) -> Arc<Self> {
        Self::new_with_seals(readable, writable, mode, uid, gid, false, false, false)
    }

    /// 创建 `memfd_create(2)` 使用的匿名文件，并按参数初始化 seal 状态。
    pub fn new_memfd(
        readable: bool,
        writable: bool,
        mode: u32,
        uid: u32,
        gid: u32,
        allow_sealing: bool,
        noexec_seal: bool,
    ) -> Arc<Self> {
        Self::new_with_seals(
            readable,
            writable,
            mode,
            uid,
            gid,
            allow_sealing,
            noexec_seal,
            true,
        )
    }

    /// 统一构造 O_TMPFILE 与 memfd；`is_memfd` 决定是否开放 memfd 专有接口。
    fn new_with_seals(
        readable: bool,
        writable: bool,
        mode: u32,
        uid: u32,
        gid: u32,
        allow_sealing: bool,
        noexec_seal: bool,
        is_memfd: bool,
    ) -> Arc<Self> {
        let initial_seals = (if allow_sealing { 0 } else { F_SEAL_SEAL })
            | (if noexec_seal { F_SEAL_EXEC } else { 0 });
        Arc::new(Self {
            readable,
            writable,
            mode,
            uid,
            gid,
            ino: NEXT_TMP_INO.fetch_add(1, Ordering::Relaxed),
            append: AtomicBool::new(false),
            is_memfd,
            allow_sealing,
            seals: Arc::new(AtomicU32::new(initial_seals)),
            mmap_state: Arc::new(TmpFileMmapState {
                writable_shared_mappings: Mutex::new(0),
                shared_pages: Mutex::new(BTreeMap::new()),
            }),
            inner: Arc::new(Mutex::new(TmpFileInner { data: Vec::new() })),
            offset: Mutex::new(0),
        })
    }

    /// 以 acquire 顺序读取当前 seal 位图，确保观察到最新限制。
    fn seals(&self) -> u32 {
        self.seals.load(Ordering::Acquire)
    }
}

impl File for TmpFile {
    fn readable(&self) -> bool {
        self.readable
    }

    fn writable(&self) -> bool {
        self.writable
    }

    /// 先同步共享映射脏页，再按本打开描述的独立偏移读取文件内容。
    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let mut pages = self.mmap_state.shared_pages.lock();
        let mut inner = self.inner.lock();
        TmpFileMmapBacking::flush_shared_pages(&mut pages, &mut inner);
        let mut offset = self.offset.lock();
        if *offset >= inner.data.len() {
            return Ok(0);
        }
        let end = (*offset + buf.len()).min(inner.data.len());
        let read_len = buf.write(&inner.data[*offset..end]);
        *offset += read_len;
        Ok(read_len)
    }

    /// 检查写入相关 seals 后写入内容，并刷新已有共享映射页。
    fn write(&self, buf: UserBuffer) -> SyscallRet {
        let bytes = buf.read_to_vec();
        let mut pages = self.mmap_state.shared_pages.lock();
        let mut inner = self.inner.lock();
        TmpFileMmapBacking::flush_shared_pages(&mut pages, &mut inner);
        let mut offset = self.offset.lock();
        let seals = self.seals();
        if seals & WRITE_SEALS != 0 {
            return Err(SysErrNo::EPERM);
        }
        if self.append.load(Ordering::Acquire) {
            *offset = inner.data.len();
        }
        let end = (*offset).checked_add(bytes.len()).ok_or(SysErrNo::EFBIG)?;
        if end > inner.data.len() && seals & F_SEAL_GROW != 0 {
            return Err(SysErrNo::EPERM);
        }
        if end > inner.data.len() {
            inner.data.resize(end, 0);
        }
        inner.data[*offset..end].copy_from_slice(&bytes);
        *offset = end;
        TmpFileMmapBacking::refresh_shared_pages(&mut pages, &inner);
        Ok(bytes.len())
    }

    /// 调整文件长度；扩展或收缩分别受 `F_SEAL_GROW`/`F_SEAL_SHRINK` 约束。
    fn truncate(&self, size: usize) -> SyscallRet {
        let mut pages = self.mmap_state.shared_pages.lock();
        let mut inner = self.inner.lock();
        TmpFileMmapBacking::flush_shared_pages(&mut pages, &mut inner);
        let mut offset = self.offset.lock();
        let seals = self.seals();
        if (size < inner.data.len() && seals & F_SEAL_SHRINK != 0)
            || (size > inner.data.len() && seals & F_SEAL_GROW != 0)
        {
            return Err(SysErrNo::EPERM);
        }
        inner.data.resize(size, 0);
        if *offset > size {
            *offset = size;
        }
        TmpFileMmapBacking::refresh_shared_pages(&mut pages, &inner);
        Ok(0)
    }

    /// 实现 memfd 支持的保留大小和打洞操作；打洞区域保留文件长度并清零。
    fn fallocate(&self, mode: u32, offset: usize, len: usize) -> SyscallRet {
        if !self.is_memfd {
            return Err(SysErrNo::EOPNOTSUPP);
        }
        if mode & !(FALLOC_FL_KEEP_SIZE | FALLOC_FL_PUNCH_HOLE) != 0 {
            return Err(SysErrNo::EOPNOTSUPP);
        }
        if mode & FALLOC_FL_PUNCH_HOLE != 0 && mode & FALLOC_FL_KEEP_SIZE == 0 {
            return Err(SysErrNo::EINVAL);
        }
        let end = offset.checked_add(len).ok_or(SysErrNo::EFBIG)?;
        let mut pages = self.mmap_state.shared_pages.lock();
        let mut inner = self.inner.lock();
        TmpFileMmapBacking::flush_shared_pages(&mut pages, &mut inner);
        let seals = self.seals();
        if mode & FALLOC_FL_PUNCH_HOLE != 0 {
            if seals & WRITE_SEALS != 0 {
                return Err(SysErrNo::EPERM);
            }
            let punch_end = end.min(inner.data.len());
            if offset < punch_end {
                for byte in &mut inner.data[offset..punch_end] {
                    *byte = 0;
                }
            }
            TmpFileMmapBacking::refresh_shared_pages(&mut pages, &inner);
            return Ok(0);
        }
        if mode & FALLOC_FL_KEEP_SIZE == 0 && end > inner.data.len() {
            if seals & F_SEAL_GROW != 0 {
                return Err(SysErrNo::EPERM);
            }
            inner.data.resize(end, 0);
        }
        TmpFileMmapBacking::refresh_shared_pages(&mut pages, &inner);
        Ok(0)
    }

    /// 返回匿名普通文件的元数据；未链接文件的硬链接数固定为零。
    fn fstat(&self) -> Kstat {
        let inner = self.inner.lock();
        Kstat {
            st_ino: self.ino,
            st_mode: StMode::FREG.bits() | (self.mode & 0o7777),
            // Anonymous tmpfiles have no directory entry until linkat()
            // materializes them, so Linux reports link count 0 here.
            st_nlink: 0,
            st_uid: self.uid,
            st_gid: self.gid,
            st_size: inner.data.len() as isize,
            st_blksize: 512,
            st_blocks: ((inner.data.len() + 511) / 512) as isize,
            ..Kstat::default()
        }
    }

    /// 按起点、当前位置或文件末尾计算新的非负文件偏移。
    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet {
        let inner = self.inner.lock();
        let mut current_offset = self.offset.lock();
        let base = match whence {
            0 => 0isize,
            1 => *current_offset as isize,
            2 => inner.data.len() as isize,
            _ => return Err(SysErrNo::EINVAL),
        };
        let new_offset = base.checked_add(offset).ok_or(SysErrNo::EINVAL)?;
        if new_offset < 0 {
            return Err(SysErrNo::EINVAL);
        }
        *current_offset = new_offset as usize;
        Ok(*current_offset)
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

    fn set_append(&self, append: bool) -> SysResult {
        self.append.store(append, Ordering::Release);
        Ok(())
    }

    fn get_seals(&self) -> Result<u32, SysErrNo> {
        if self.is_memfd {
            Ok(self.seals())
        } else {
            Err(SysErrNo::EINVAL)
        }
    }

    /// 原子地追加 seal；安装写 seal 前拒绝仍存在的可写共享映射。
    fn add_seals(&self, seals: u32) -> SysResult {
        if !self.is_memfd || seals & !SUPPORTED_SEALS != 0 {
            return Err(SysErrNo::EINVAL);
        }
        if !self.allow_sealing {
            return Err(SysErrNo::EPERM);
        }

        // Linux rejects F_SEAL_WRITE while a shared writable mapping exists.
        // Serialize that check with mmap_backing() so a new mapping cannot race
        // the seal installation between the check and the CAS below.
        let mappings = self.mmap_state.writable_shared_mappings.lock();
        if seals & F_SEAL_WRITE != 0 && *mappings != 0 {
            return Err(SysErrNo::EBUSY);
        }
        let mut current = self.seals();
        loop {
            if current & F_SEAL_SEAL != 0 {
                return Err(SysErrNo::EPERM);
            }
            let updated = current | seals;
            match self.seals.compare_exchange_weak(
                current,
                updated,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }

    /// 以新的访问标志重新打开同一个 memfd，共享内容和 seal，但拥有独立偏移。
    fn reopen(
        &self,
        readable: bool,
        writable: bool,
        append: bool,
    ) -> Result<Arc<dyn File>, SysErrNo> {
        if !self.is_memfd {
            return Err(SysErrNo::EINVAL);
        }
        Ok(Arc::new(Self {
            readable,
            writable,
            mode: self.mode,
            uid: self.uid,
            gid: self.gid,
            ino: self.ino,
            append: AtomicBool::new(append),
            is_memfd: true,
            allow_sealing: self.allow_sealing,
            seals: Arc::clone(&self.seals),
            mmap_state: Arc::clone(&self.mmap_state),
            inner: Arc::clone(&self.inner),
            offset: Mutex::new(0),
        }))
    }

    /// 创建 memfd 的 mmap 后端，并登记可写共享映射数量以协调 seals。
    fn mmap_backing(&self, shared: bool, writable: bool) -> Result<Arc<dyn MmapBacking>, SysErrNo> {
        if !self.is_memfd {
            return Err(SysErrNo::EINVAL);
        }
        let shared_writable = shared && writable;
        if shared_writable {
            let mut mappings = self.mmap_state.writable_shared_mappings.lock();
            if self.seals() & (F_SEAL_WRITE | F_SEAL_FUTURE_WRITE) != 0 {
                return Err(SysErrNo::EPERM);
            }
            *mappings += 1;
        }
        Ok(Arc::new(TmpFileMmapBacking {
            state: Arc::clone(&self.mmap_state),
            seals: Arc::clone(&self.seals),
            inner: Arc::clone(&self.inner),
            shared,
            writable_shared: shared_writable,
        }))
    }
}

/// 将分散的用户缓冲区拼接为独立内核快照，避免锁内持有用户切片。
trait UserBufferExt {
    fn read_to_vec(&self) -> Vec<u8>;
}

impl UserBufferExt for UserBuffer {
    /// Snapshot user buffer slices before taking the file lock, so writes can
    /// resize the backing Vec without keeping references into UserBuffer.
    fn read_to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for slice in self.buffers.iter() {
            bytes.extend_from_slice(slice);
        }
        bytes
    }
}
