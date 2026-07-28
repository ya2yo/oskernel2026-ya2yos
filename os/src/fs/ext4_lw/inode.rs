//! EXT4 inode adapter.
//!
//! 本文件把 `lwext4_rust::Ext4File` 适配为 Ya2yOS VFS [`Inode`] trait。
//! lwext4 wrapper 主要暴露路径式 API，因此 `Ext4Inode` 除了保存底层
//! `Ext4File` 外，还维护同一 inode 的路径别名，用于 hard link / rename 后继续
//! 找到一个可用路径。

use log::{debug, warn};
use lwext4_rust::{
    bindings::{ext4_inode_stat, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, SEEK_SET},
    Ext4File, InodeTypes,
};

use super::{TaskMutex, EXT4_OP_LOCK};
use crate::{
    fs::{
        patch_dynamic_link_file_bytes, FsIndex, Inode, InodeType, Kstat, MountFlags, OpenFlags,
        String, FILE_PAGE_CACHE, MNT_TABLE,
    },
    sync::SyncUnsafeCell,
    utils::{SysErrNo, SysResult, SyscallRet},
};

use alloc::{format, string::ToString, vec};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::RwLock;

use lwext4_rust::file::{discard_path_cache, read_cached_at, write_cached_at, OsDirent};

/// 防止符号链接死循环的最大跳转次数。
const MAX_LOOPTIMES: usize = 5;
/// `find_from_cached_parent()` has already resolved every component preceding
/// the final name.  Use a private sentinel to skip the compatibility scan for
/// intermediate symlinks after that final lookup misses.
const SKIP_INTERMEDIATE_SYMLINK_RETRY: usize = usize::MAX;
const QUOTA_RESERVE_GRANULARITY: usize = 64 * 1024;
const UNKNOWN_FILE_SIZE: usize = usize::MAX;

/// EXT4 inode 的 VFS 包装。
///
/// `Ext4Inode` 是 VFS 层看到的 inode 对象，内部用 `Ext4File` 调用 lwext4。
/// 由于底层接口以 path 为主，不是以稳定 inode handle 为主，因此这里还保存路径
/// alias，并在必要时重新打开到仍然存在的路径。
pub struct Ext4Inode {
    inner: SyncUnsafeCell<Ext4InodeInner>,
    /// Serializes transitions of this inode's pathname, cache policy and
    /// quota reservation.  A resident byte-cache write holds this lock but
    /// does not need the mount-wide lwext4 lock, mirroring Linux's inode-level
    /// write serialization while preserving lwext4's global slow-path gate.
    write_state: TaskMutex,
    /// 文件类型在创建后不会改变；将它放在可变 lwext4 状态之外，让纯类型
    /// 查询不必争用全局 EXT4 操作锁。
    inode_type: InodeType,
    /// The path is read by page-cache and fd bookkeeping on every access. It
    /// changes only after a successful rename or alias recovery, so keep a
    /// VFS-side mirror instead of taking the global lwext4 lock for `path()`.
    path: RwLock<Arc<str>>,
    /// File-backed mmap faults check EOF for every page. Once lwext4 has
    /// established a regular file's size, serve that immutable-until-write
    /// value without serializing on its global operation lock.
    known_size: AtomicUsize,
    /// Upper bound already charged to the loop-mounted filesystem quota.
    /// `write_state` serializes updates, while the atomic load permits the
    /// cache-only write fast path to reject unreserved extensions before it
    /// reaches lwext4 or the mount table.
    quota_reserved: AtomicUsize,
    /// Delayed-unlink files must pin their cache before accepting another
    /// write.  Keep them on the established slow path, which performs that
    /// pinning, rather than allowing a FIFO eviction to recreate an unlinked
    /// pathname.
    delayed: AtomicBool,
    /// `(st_dev, st_ino)` is immutable while this VFS inode is live.  Keep
    /// the value returned by the initial pathname lookup so FsIndex does not
    /// immediately re-enter lwext4 merely to assign its cache key.
    inode_identity: Option<(usize, usize)>,
    /// Repeated `stat(2)` calls on an unchanged regular source or artifact
    /// otherwise serialize on lwext4's global path-based metadata lookup.
    /// Directories and special nodes are intentionally excluded because their
    /// metadata changes as children are created or removed.
    stat_cache: RwLock<Option<Kstat>>,
}

/// `Ext4Inode` 的可变内部状态。
///
/// `SyncUnsafeCell` 包住该结构后，外部方法可以在 `&self` 下调用 lwext4 的可变接口。
/// 这要求调用方遵守文件系统层的锁/单核执行假设，不要并发修改同一个 inode 状态。
pub struct Ext4InodeInner {
    /// lwext4 wrapper 的文件/目录句柄。
    f: Ext4File,
    /// 指向同一 inode 的路径别名，用于 hard link / rename 后继续找到可用路径。
    aliases: Vec<String>,
    /// 延迟删除标志。如果为 true，在该 inode 被 Drop 时会从磁盘删除对应文件。
    delay: bool,
}

/// `find()` only needs lwext4 serialization for the metadata query itself.
/// Constructing the VFS wrapper allocates Rust-side state and must stay outside
/// the global operation lock so parallel Cargo lookups can hand it over sooner.
enum Ext4FindResult {
    Dir(ext4_inode_stat),
    File(ext4_inode_stat),
    SymLink(ext4_inode_stat),
    Missing,
}

unsafe impl Send for Ext4Inode {}
unsafe impl Sync for Ext4Inode {}

impl Ext4Inode {
    /// 创建一个新的 `Ext4Inode` 实例。
    ///
    /// - `path`: 文件在 EXT4 内部的路径
    /// - `types`: 文件类型（文件、目录、链接等）
    pub fn new(path: &str, types: InodeTypes) -> Self {
        Self::new_with_lookup_stat(path, types, None)
    }

    /// Build an inode from a lookup that has already performed
    /// `ext4_stat_get()`.  Reusing the metadata avoids a second serialized
    /// lookup when `FsIndex` records the inode identity and, for regular
    /// files, when the first page-cache access asks for the file size.
    fn new_with_stat(path: &str, types: InodeTypes, stat: ext4_inode_stat) -> Self {
        Self::new_with_lookup_stat(path, types, Some(stat))
    }

    fn new_with_lookup_stat(
        path: &str,
        types: InodeTypes,
        lookup_stat: Option<ext4_inode_stat>,
    ) -> Self {
        let inode_type = as_inode_type(types.clone());
        let inode_identity = lookup_stat
            .as_ref()
            .and_then(|stat| (stat.st_ino != 0).then_some((stat.st_dev, stat.st_ino)));
        let (known_size, stat_cache) = if inode_type == InodeType::File {
            match lookup_stat {
                Some(stat) => (stat.st_size as usize, Some(Self::kstat_from_ext4(stat))),
                None => (UNKNOWN_FILE_SIZE, None),
            }
        } else {
            (UNKNOWN_FILE_SIZE, None)
        };
        Ext4Inode {
            inode_type,
            write_state: TaskMutex::new(),
            path: RwLock::new(Arc::from(path)),
            known_size: AtomicUsize::new(known_size),
            quota_reserved: AtomicUsize::new(0),
            delayed: AtomicBool::new(false),
            inode_identity,
            stat_cache: RwLock::new(stat_cache),
            inner: SyncUnsafeCell::new(Ext4InodeInner {
                f: Ext4File::new(path, types),
                aliases: vec![path.to_string()],
                delay: false,
            }),
        }
    }

    /// Convert lwext4 metadata into the VFS representation while preserving
    /// the timestamp compatibility normalization used by `fstat()`.
    fn kstat_from_ext4(stat: ext4_inode_stat) -> Kstat {
        let mut tmp_stat = stat;
        if tmp_stat.st_atime > (1 << 32) || tmp_stat.st_mtime > (1 << 32) {
            tmp_stat.st_ctime &= 0xFFFF_FFFF;
            tmp_stat.st_atime &= 0xFFFF_FFFF;
            tmp_stat.st_mtime &= 0xFFFF_FFFF;
        }
        Kstat {
            st_dev: stat.st_dev,
            st_ino: stat.st_ino,
            st_mode: stat.st_mode,
            st_nlink: stat.st_nlink,
            st_uid: stat.st_uid,
            st_gid: stat.st_gid,
            st_size: stat.st_size,
            st_blksize: stat.st_blksize,
            st_blocks: stat.st_blocks,
            st_atime: tmp_stat.st_atime,
            st_ctime: tmp_stat.st_ctime,
            st_mtime: tmp_stat.st_mtime,
            ..Kstat::default()
        }
    }

    /// 记录一个仍可指向当前 inode 的路径别名。
    ///
    /// hard link / rename 之后，原始路径可能失效，但 fd 仍应能继续访问同一个文件。
    /// alias 列表供 `recover_live_path()` 在底层 path 操作失败时兜底。
    fn add_alias_path(&self, path: &str) {
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        if inner.aliases.iter().all(|alias| alias != path) {
            inner.aliases.push(path.to_string());
        }
    }

    /// 返回当前 `Ext4File` 记录的路径。
    ///
    /// 热路径上不主动调用 `check_inode_exist()`，避免每次 `fstat/fmode/path` 都触发
    /// 底层目录项查找；如果后续 lwext4 操作失败，再由 `recover_live_path()` 扫 alias。
    fn live_path(inner: &mut Ext4InodeInner) -> String {
        inner.f.path().into_string().unwrap()
    }

    #[inline]
    fn cached_path(&self) -> String {
        String::from(self.path.read().as_ref())
    }

    #[inline]
    fn cached_page_cache_path(&self) -> Arc<str> {
        self.path.read().clone()
    }

    #[inline]
    fn update_cached_path(&self, path: &str) {
        *self.path.write() = Arc::from(path);
    }

    #[inline]
    fn known_size(&self) -> Option<usize> {
        match self.known_size.load(Ordering::Acquire) {
            UNKNOWN_FILE_SIZE => None,
            size => Some(size),
        }
    }

    #[inline]
    fn update_known_size(&self, size: usize) {
        self.known_size.store(size, Ordering::Release);
    }

    #[inline]
    fn cached_stat(&self) -> Option<Kstat> {
        if self.inode_type == InodeType::File {
            *self.stat_cache.read()
        } else {
            None
        }
    }

    #[inline]
    fn update_cached_stat(&self, stat: Kstat) {
        if self.inode_type == InodeType::File {
            *self.stat_cache.write() = Some(stat);
        }
    }

    #[inline]
    fn invalidate_cached_stat(&self) {
        if self.inode_type == InodeType::File {
            *self.stat_cache.write() = None;
        }
    }

    #[inline]
    fn stat_with_known_size(&self, mut stat: Kstat) -> Kstat {
        if let Some(size) = self.known_size() {
            stat.st_size = size as isize;
        }
        stat
    }

    /// 在当前路径失效后尝试从 alias 列表恢复一个可用路径。
    ///
    /// 这是 rename/hard link 后 fd 继续可用的兜底路径。只有底层元数据操作失败时才调用，
    /// 避免把普通热路径变成重复的 ext4 路径存在性检查。
    fn recover_live_path(&self, inner: &mut Ext4InodeInner) -> String {
        let current = inner.f.path().into_string().unwrap();
        let types = inner.f.types();
        if inner.f.check_inode_exist(&current, types.clone()) {
            self.update_cached_path(&current);
            return current;
        }

        for alias in inner.aliases.clone() {
            if inner.f.check_inode_exist(&alias, types.clone()) {
                let _ = inner.f.file_close();
                inner.f = Ext4File::new(&alias, types.clone());
                self.update_cached_path(&alias);
                return alias;
            }
        }
        current
    }

    /// Find the first symbolic link before the final component of an absolute
    /// pathname.  lwext4's path lookups can resolve a final link, but a cold
    /// lookup through Debian's `/lib -> usr/lib` reports ENOENT once the VFS
    /// parent cache has been reclaimed.  Intermediate links must always be
    /// followed, including for operations that preserve a final link.
    fn first_intermediate_symlink(&self, path: &str) -> Option<String> {
        let components: Vec<&str> = path
            .split('/')
            .filter(|component| !component.is_empty())
            .collect();
        if components.len() < 2 {
            return None;
        }

        let _ext4 = EXT4_OP_LOCK.lock();
        let file = &mut self.inner.get_unchecked_mut().f;
        let mut prefix = String::new();
        for component in &components[..components.len() - 1] {
            prefix.push('/');
            prefix.push_str(component);
            if file.is_symlink(&prefix) {
                return Some(prefix);
            }
        }
        None
    }

    /// Replace an intermediate symlink in `path` with its target.
    fn resolve_intermediate_symlink(&self, path: &str) -> Result<Option<String>, SysErrNo> {
        let Some(link_path) = self.first_intermediate_symlink(path) else {
            return Ok(None);
        };
        let mut link_buf = [0u8; 256];
        let link_buf_len = link_buf.len();
        let link = Ext4Inode::new(&link_path, InodeTypes::EXT4_DE_SYMLINK);
        let link_len = link.read_link(&mut link_buf, link_buf_len)?;
        let target = core::str::from_utf8(&link_buf[..link_len]).map_err(|_| SysErrNo::ENOENT)?;
        let resolved_target = if target.starts_with('/') {
            join_path("/", target)
        } else {
            join_path(&link_path, target)
        };
        let suffix = path.strip_prefix(&link_path).ok_or(SysErrNo::ENOENT)?;
        Ok(Some(format!(
            "{}{}",
            resolved_target.trim_end_matches('/'),
            suffix
        )))
    }
}

impl Inode for Ext4Inode {
    /// 获取普通文件大小。
    ///
    /// 目录和其他非普通文件当前返回 0；普通文件需要按 lwext4 API 重新打开后读取 size。
    fn size(&self) -> usize {
        if self.inode_type != InodeType::File {
            return 0;
        }
        if let Some(size) = self.known_size() {
            return size;
        }

        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        if let Some(size) = self.known_size() {
            return size;
        }
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open_read_only(&path);
        let size = file.file_size() as usize;
        self.update_known_size(size);
        size
    }
    /// 在当前文件系统中创建一个新 inode。
    ///
    /// `path` 必须是绝对路径。目录通过 `dir_mk()` 创建，普通文件通过
    /// `file_open(O_CREAT|O_TRUNC)` 创建后立即关闭。目标已存在时返回 `EEXIST`，
    /// 用于承载 Linux `O_CREAT|O_EXCL` 语义。
    ///
    /// 新文件的描述符关闭不立即刷新全局 block cache；创建路径通常还会设置
    /// mode/owner 或写入内容，由上层在完整操作结束后统一同步。
    fn create(&self, path: &str, ty: InodeType) -> Result<Arc<dyn Inode>, SysErrNo> {
        let types = as_ext4_de_type(ty);
        // Drop closes the underlying handle under EXT4_OP_LOCK.  Construct it
        // before the guard so error paths release the guard before Drop runs.
        let nf = Ext4Inode::new(path, types.clone());
        let _ext4 = EXT4_OP_LOCK.lock();
        let file = &mut self.inner.get_unchecked_mut().f;

        if file.check_inode_exist(path, types.clone()) {
            return Err(SysErrNo::EEXIST);
        }

        let nfile = &mut nf.inner.get_unchecked_mut().f;
        if types == InodeTypes::EXT4_DE_DIR {
            if let Err(e) = nfile.dir_mk(path) {
                return Err(SysErrNo::from(e));
            }
        } else if let Err(e) = nfile.file_open(path, O_RDWR | O_CREAT | O_TRUNC) {
            return Err(SysErrNo::from(e));
        } else {
            nfile.file_close_without_cache_flush()?;
        }
        Ok(Arc::new(nf))
    }

    /// 创建内核维护的目录项。
    ///
    /// `ext4_dir_mk()` 自身已经完成“存在则打开，不存在则创建”的路径处理；
    /// proc 目录由 PID 分配器保证名称唯一，因此无需再由 VFS 先执行一次
    /// `check_inode_exist()`。
    fn create_dir_fast(&self, path: &str) -> Result<Arc<dyn Inode>, SysErrNo> {
        let nf = Ext4Inode::new(path, InodeTypes::EXT4_DE_DIR);
        let _ext4 = EXT4_OP_LOCK.lock();
        let nfile = &mut nf.inner.get_unchecked_mut().f;
        nfile.dir_mk(path).map_err(SysErrNo::from)?;
        Ok(Arc::new(nf))
    }

    /// 返回 inode 类型。
    ///
    /// 类型来自 `Ext4File` 构造时记录的 lwext4 类型，避免为了类型判断再次走路径查询。
    fn types(&self) -> InodeType {
        self.inode_type
    }

    /// 从指定偏移量读取数据到缓冲区。
    ///
    /// 动态链接文件可能需要按路径 patch 内容，因此读取完成后会调用
    /// `patch_dynamic_link_file_bytes()` 做兼容修补。
    fn read_at(&self, off: usize, buf: &mut [u8]) -> SyscallRet {
        if buf.is_empty() {
            return Ok(0);
        }
        // The delayed byte cache has its own synchronization and contains
        // every visible byte for its dense, non-sparse inode.  Check it
        // before taking the mount-wide lwext4 guard: concurrent compiler
        // readers otherwise queue behind unrelated block I/O merely to copy
        // data that is already resident in kernel memory.
        let cached_path = self.cached_page_cache_path();
        if let Some(r) = read_cached_at(&cached_path, off, buf) {
            #[cfg(feature = "perf")]
            {
                crate::utils::perf::record_ext4_read(r);
                crate::utils::perf::record_ext4_byte_cache_read_hit(r);
            }
            patch_dynamic_link_file_bytes(&cached_path, off, &mut buf[..r]);
            return Ok(r);
        }
        // Keep the lwext4 guard around cache/descriptor access only. The
        // compatibility patch below mutates an already-read buffer and does
        // not touch lwext4, so doing it under the global guard needlessly
        // extends contention for concurrent readers.
        let (path, r, byte_cache_hit) = {
            let _ext4 = EXT4_OP_LOCK.lock_for_read();
            let inner = self.inner.get_unchecked_mut();
            let path = Self::live_path(inner);
            let file = &mut inner.f;
            // Read-back caches may contain dirty bytes which are not on disk yet.
            // Check them first, then use a direct ext4 read for the common cold
            // read-only case. The latter avoids creating a whole-file write-back
            // cache and avoids a separate fseek for every VFS read.
            let (r, byte_cache_hit) = if let Some(r) = read_cached_at(&path, off, buf) {
                (r, true)
            } else {
                file.file_open_read_only(&path).map_err(SysErrNo::from)?;
                (file.file_read_at(off, buf).map_err(SysErrNo::from)?, false)
            };
            (path, r, byte_cache_hit)
        };
        #[cfg(not(feature = "perf"))]
        let _ = byte_cache_hit;
        #[cfg(feature = "perf")]
        {
            crate::utils::perf::record_ext4_read(r);
            if byte_cache_hit {
                crate::utils::perf::record_ext4_byte_cache_read_hit(r);
            }
        }
        // lwext4's ext4_fread() only reads blocks and advances the descriptor
        // position; atime changes go through the explicit set_timestamps()
        // path. Keeping the immutable regular-file stat cache here avoids
        // turning the next fstat() into another serialized metadata lookup.
        patch_dynamic_link_file_bytes(&path, off, &mut buf[..r]);
        Ok(r)
    }

    /// 从指定偏移量写入数据。
    fn write_at(&self, off: usize, buf: &[u8]) -> SyscallRet {
        // The state lock keeps the path, cache policy and quota reservation
        // stable while an in-memory write bypasses `EXT4_OP_LOCK`.  Slow paths
        // retain the original global serialization before entering lwext4.
        let _write_state = self.write_state.lock();
        let end = off.checked_add(buf.len()).ok_or(SysErrNo::EFBIG)?;
        let cached_path = self.cached_path();
        if !self.delayed.load(Ordering::Acquire)
            && end <= self.quota_reserved.load(Ordering::Acquire)
        {
            if let Some(current_size) = self.known_size() {
                if let Some(result) = write_cached_at(cached_path.as_ref(), off, buf) {
                    let written = result.map_err(SysErrNo::from)?;
                    let end = off.checked_add(written).ok_or(SysErrNo::EFBIG)?;
                    self.update_known_size(current_size.max(end));
                    self.invalidate_cached_stat();
                    return Ok(written);
                }
            }
        }

        let _ext4 = EXT4_OP_LOCK.lock_for_write();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let delayed = inner.delay;
        let file = &mut inner.f;
        #[cfg(feature = "perf")]
        let open_phase =
            crate::utils::perf::Ext4WritePhaseGuard::new(crate::utils::perf::Ext4WritePhase::Open);
        file.ensure_open(O_RDWR).map_err(SysErrNo::from)?;
        #[cfg(feature = "perf")]
        drop(open_phase);
        if delayed {
            // Keep an unlinked-but-open temporary file's cache alive until
            // its last fd closes; the FIFO cannot otherwise distinguish it
            // from an idle cache and will repeatedly evict/rebuild it.
            file.pin_write_back_cache();
        }
        #[cfg(feature = "perf")]
        let quota_phase =
            crate::utils::perf::Ext4WritePhaseGuard::new(crate::utils::perf::Ext4WritePhase::Quota);
        let current_size = self
            .known_size()
            .unwrap_or_else(|| file.file_size() as usize);
        let previous_reserved = self.quota_reserved.load(Ordering::Acquire);
        let reservation = if end > previous_reserved {
            let target = if end <= QUOTA_RESERVE_GRANULARITY {
                end
            } else {
                end.checked_add(QUOTA_RESERVE_GRANULARITY - 1)
                    .map(|value| value / QUOTA_RESERVE_GRANULARITY * QUOTA_RESERVE_GRANULARITY)
                    .ok_or(SysErrNo::EFBIG)?
            };
            let previous = previous_reserved.max(current_size);
            let mut mount_table = MNT_TABLE.lock();
            if let Err(error) = mount_table.reserve_write(&path, previous, target) {
                // The final chunk may be smaller than the reservation unit.
                // Charge only this write before reporting ENOSPC so callers
                // can consume the mount exactly up to its real limit.
                if target == end || error != SysErrNo::ENOSPC {
                    if error == SysErrNo::ENOSPC {
                        file.defer_close_flush();
                    }
                    return Err(error);
                }
                if let Err(error) = mount_table.reserve_write(&path, previous, end) {
                    if error == SysErrNo::ENOSPC {
                        file.defer_close_flush();
                    }
                    return Err(error);
                }
                self.quota_reserved.store(end, Ordering::Release);
                Some((previous, end))
            } else {
                self.quota_reserved.store(target, Ordering::Release);
                Some((previous, target))
            }
        } else {
            None
        };
        #[cfg(feature = "perf")]
        drop(quota_phase);
        #[cfg(feature = "perf")]
        let data_phase =
            crate::utils::perf::Ext4WritePhaseGuard::new(crate::utils::perf::Ext4WritePhase::Data);
        if off > current_size {
            // A write beyond EOF creates a sparse range. The whole-file cache
            // tracks bytes only and would otherwise materialize that range.
            file.disable_write_back_cache().map_err(SysErrNo::from)?;
        }
        let written = match file.file_write_at(off, buf) {
            Ok(written) => written,
            Err(err) => {
                if SysErrNo::from(err) == SysErrNo::ENOSPC {
                    file.defer_close_flush();
                }
                if let Some((previous, target)) = reservation {
                    MNT_TABLE
                        .lock()
                        .rollback_reservation(&path, previous, target);
                    self.quota_reserved
                        .store(previous_reserved, Ordering::Release);
                }
                return Err(SysErrNo::from(err));
            }
        };
        #[cfg(feature = "perf")]
        drop(data_phase);
        let end = off.checked_add(written).ok_or(SysErrNo::EFBIG)?;
        self.update_known_size(current_size.max(end));
        self.invalidate_cached_stat();
        Ok(written)
    }

    /// 截断文件到指定长度。
    ///
    /// 成功后失效文件页缓存，避免 mmap/page cache 继续暴露旧大小或旧内容。
    fn truncate(&self, size: usize) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open(&path, O_RDWR).map_err(SysErrNo::from)?;

        file.file_truncate(size as u64).map_err(SysErrNo::from)?;
        self.update_known_size(size);
        self.invalidate_cached_stat();
        FILE_PAGE_CACHE.invalidate_path(&path);
        Ok(0)
    }

    /// 重命名当前 inode 对应的路径。
    ///
    /// 成功后把新路径加入 alias，并将内部 `Ext4File` 切换到新路径，减少后续元数据操作
    /// 依赖 fallback 恢复路径的次数。
    fn rename(&self, path: &str, new_path: &str) -> SyscallRet {
        if path == new_path {
            return Ok(0);
        }

        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock_for_rename();
        let inner = self.inner.get_unchecked_mut();
        let types = inner.f.types();
        let active_path = inner.f.path().into_string().unwrap();

        // Rustc publishes rmeta/rlib files by renaming a populated temporary
        // path.  Write back and detach its path-keyed cache while the source
        // still exists; otherwise a later close can recreate the temp file and
        // leave the published destination stale.  Do not make rename flush the
        // whole mount's block cache: rename preserves the inode and Linux does
        // not give it fsync durability semantics.
        inner
            .f
            .write_back_and_discard_path_cache()
            .map_err(SysErrNo::from)?;
        // The preceding helper has already handled byte-cache write-back.
        // Closing with the normal helper would call ext4_cache_flush() a
        // second time, again serializing every dirty block on this mount.
        inner
            .f
            .file_close_without_cache_flush()
            .map_err(SysErrNo::from)?;
        inner
            .f
            .file_rename(path, new_path)
            .map_err(SysErrNo::from)?;

        // A successful directory-entry move must not leave an orphaned
        // write-back entry for either pathname.  In particular, stale target
        // state could otherwise overwrite Rustc's newly published artifact.
        discard_path_cache(path);
        discard_path_cache(new_path);

        inner.aliases.retain(|alias| alias.as_str() != path);
        if inner.aliases.iter().all(|alias| alias != new_path) {
            inner.aliases.push(new_path.to_string());
        }
        inner.f = Ext4File::new(new_path, types);
        self.update_cached_path(new_path);
        self.invalidate_cached_stat();
        FILE_PAGE_CACHE.invalidate_path(&active_path);
        FILE_PAGE_CACHE.invalidate_path(path);
        FILE_PAGE_CACHE.invalidate_path(new_path);
        Ok(0)
    }

    /// 创建硬链接：`new_path` 指向 `old_path` 相同的 inode。
    ///
    /// 成功后把新路径加入 alias，以便原路径 unlink 后已打开 fd 仍有可用路径。
    fn hard_link(&self, old_path: &str, new_path: &str) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let file = &mut inner.f;
        let ret = file
            .file_hardlink(old_path, new_path)
            .map_or(Err(SysErrNo::ENOENT), |_| Ok(0));
        if ret.is_ok() {
            if inner.aliases.iter().all(|alias| alias != new_path) {
                inner.aliases.push(new_path.to_string());
            }
            self.invalidate_cached_stat();
        }
        ret
    }

    /// 设置 inode 时间戳。
    fn set_timestamps(
        &self,
        atime: Option<u64>,
        mtime: Option<u64>,
        ctime: Option<u64>,
    ) -> SyscallRet {
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        let ret = file.set_time(atime, mtime, ctime).map_err(SysErrNo::from);
        if ret.is_ok() {
            self.invalidate_cached_stat();
        }
        ret
    }

    /// 将 lwext4 文件缓存刷新到磁盘。
    fn sync(&self) {
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        inner.f.file_cache_flush();
    }

    /// 一次性读取整个文件内容。
    ///
    /// 普通文件直接读取全部字节；符号链接会先解析链接目标，再递归读取真实文件。
    fn read_all(&self) -> Result<Vec<u8>, SysErrNo> {
        // 先提取 path 和类型，避免后续访问 self.inner 时产生重叠借用
        let (file_type, path_str) = {
            let _ext4 = EXT4_OP_LOCK.lock();
            let inner = self.inner.get_unchecked_mut();
            let path = Self::live_path(inner);
            let file_type = as_inode_type(inner.f.types());
            (file_type, path)
        };

        if file_type == InodeType::File {
            let _ext4 = EXT4_OP_LOCK.lock();
            let file = &mut self.inner.get_unchecked_mut().f;
            file.file_open_read_only(&path_str)
                .map_err(SysErrNo::from)?;
            let size = file.file_size() as usize;
            self.update_known_size(size);
            let mut buf: Vec<u8> = vec![0; size];
            let r = if let Some(r) = read_cached_at(&path_str, 0, buf.as_mut_slice()) {
                Ok(r)
            } else {
                file.file_read_at(0, buf.as_mut_slice())
            };
            if let Err(e) = r {
                Err(SysErrNo::from(e))
            } else {
                patch_dynamic_link_file_bytes(&path_str, 0, buf.as_mut_slice());
                Ok(buf)
            }
        } else if file_type == InodeType::SymLink {
            // 读取符号链接目标路径
            let mut real_path_buf = [0u8; 256];
            let link_file = Ext4Inode::new(&path_str, InodeTypes::EXT4_DE_SYMLINK);
            link_file.read_link(&mut real_path_buf, 256)?;
            let end = real_path_buf
                .iter()
                .position(|v| *v == 0)
                .unwrap_or(real_path_buf.len());
            let file_path =
                core::str::from_utf8(&real_path_buf[..end]).map_err(|_| SysErrNo::EINVAL)?;
            // 处理绝对/相对符号链接
            let next_path = if file_path.starts_with('/') {
                file_path.to_string()
            } else {
                join_path(&path_str, file_path)
            };
            // 通过 find 递归解析符号链接，然后读取目标文件内容
            let real_file = self.find(&next_path, OpenFlags::O_RDONLY, 0)?;
            real_file.read_all()
        } else {
            // 目录或其他不支持的类型
            Err(SysErrNo::EISDIR)
        }
    }

    /// 在路径中查找节点，支持递归解析符号链接。
    ///
    /// lwext4 没有暴露当前父目录句柄下的相对子项查找接口，因此这里仍使用 path-based
    /// `check_inode_exist()`。`O_NOFOLLOW`、`O_DIRECTORY` 和内部 `O_UNLINK` 会影响
    /// symlink/目录的返回语义。
    fn find(
        &self,
        path: &str,
        flags: OpenFlags,
        loop_times: usize,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        // log::info!("[Inode.find] origin path={}", path);
        let skip_intermediate_retry = loop_times == SKIP_INTERMEDIATE_SYMLINK_RETRY;
        let symlink_depth = if skip_intermediate_retry {
            0
        } else {
            loop_times
        };
        let result = {
            let _ext4 = EXT4_OP_LOCK.lock_for_find();
            let file = &mut self.inner.get_unchecked_mut().f;
            match file.inode_type_and_stat_at(path) {
                Ok((InodeTypes::EXT4_DE_DIR | InodeTypes::EXT4_INODE_MODE_DIRECTORY, stat)) => {
                    Ext4FindResult::Dir(stat)
                }
                Ok((InodeTypes::EXT4_DE_REG_FILE | InodeTypes::EXT4_INODE_MODE_FILE, stat)) => {
                    Ext4FindResult::File(stat)
                }
                Ok((InodeTypes::EXT4_DE_SYMLINK | InodeTypes::EXT4_INODE_MODE_SOFTLINK, stat)) => {
                    Ext4FindResult::SymLink(stat)
                }
                // Keep the existing behavior for unsupported special nodes
                // and lookup errors: callers can still retry an intermediate
                // symlink before receiving ENOENT.
                _ => Ext4FindResult::Missing,
            }
        };

        let is_symlink = match result {
            Ext4FindResult::Dir(stat) => {
                return Ok(Arc::new(Ext4Inode::new_with_stat(
                    path,
                    InodeTypes::EXT4_DE_DIR,
                    stat,
                )));
            }
            Ext4FindResult::File(stat) => {
                if flags.contains(OpenFlags::O_DIRECTORY) {
                    return Err(SysErrNo::ENOTDIR);
                }
                return Ok(Arc::new(Ext4Inode::new_with_stat(
                    path,
                    InodeTypes::EXT4_DE_REG_FILE,
                    stat,
                )));
            }
            Ext4FindResult::SymLink(_) => {
                if flags.contains(OpenFlags::O_NOFOLLOW) {
                    return Err(SysErrNo::ELOOP);
                }
                true
            }
            Ext4FindResult::Missing => false,
        };

        if !is_symlink {
            if !skip_intermediate_retry && symlink_depth < MAX_LOOPTIMES {
                if let Some(next_path) = self.resolve_intermediate_symlink(path)? {
                    return self.find(&next_path, flags, symlink_depth + 1);
                }
            }
            return Err(SysErrNo::ENOENT);
        }
        if flags.contains(OpenFlags::O_UNLINK) {
            return Ok(Arc::new(Ext4Inode::new(path, InodeTypes::EXT4_DE_SYMLINK)));
        }
        if flags.contains(OpenFlags::O_NOFOLLOW) || symlink_depth >= MAX_LOOPTIMES {
            return Err(SysErrNo::ELOOP);
        }

        let mut file_name = [0u8; 256];
        let file = Ext4Inode::new(path, InodeTypes::EXT4_DE_SYMLINK);
        file.read_link(&mut file_name, 256)?;
        let end = file_name
            .iter()
            .position(|v| *v == 0)
            .unwrap_or(file_name.len());
        let file_path = core::str::from_utf8(&file_name[..end]).unwrap();
        let next_path = if file_path.starts_with('/') {
            file_path.to_string()
        } else {
            join_path(path, file_path)
        };
        self.find(&next_path, flags, symlink_depth + 1)
    }

    /// A cached parent inode denotes an already-resolved directory.  If its
    /// direct child is absent, there cannot be an unresolved intermediate
    /// symlink in this final lookup path, so avoid the fallback prefix scan.
    /// A final symlink still follows the ordinary recursive path above.
    fn find_from_cached_parent(
        &self,
        path: &str,
        flags: OpenFlags,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        self.find(path, flags, SKIP_INTERMEDIATE_SYMLINK_RETRY)
    }
    /// 获取文件状态信息。
    ///
    /// 正常情况下直接用当前路径对应的 lwext4 句柄查询；如果路径因 rename/unlink 失效，
    /// 再尝试 `recover_live_path()`，从已记录 alias 中恢复一个仍存在的路径。
    fn fstat(&self) -> Kstat {
        if let Some(stat) = self.cached_stat() {
            return self.stat_with_known_size(stat);
        }

        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock_for_fstat();
        // Another hart may have populated the cache while this task waited
        // for lwext4.  Recheck after acquiring the guard to avoid redundant
        // metadata I/O during Cargo's parallel probes.
        if let Some(stat) = self.cached_stat() {
            return self.stat_with_known_size(stat);
        }
        let inner = self.inner.get_unchecked_mut();
        let stat = match inner.f.fstat() {
            Ok(s) => s,
            Err(rc) => {
                let _ = self.recover_live_path(inner);
                match inner.f.fstat() {
                    Ok(s) => s,
                    Err(_) => {
                        warn!(
                            "Ext4Inode::fstat: ext4_stat_get failed rc={}, path={:?}",
                            rc,
                            inner.f.path()
                        );
                        return Kstat::default();
                    }
                }
            }
        };
        let kstat = Self::kstat_from_ext4(stat);
        let cpath = inner.f.path();
        let path_str = cpath.to_str().unwrap_or("");
        let mut kstat = kstat;
        if let Some(node_type) = FsIndex::special_node_type(path_str) {
            let type_bits = node_type.mode_bits();
            kstat.st_mode = (kstat.st_mode & !0xF000) | type_bits;
        }
        self.update_cached_stat(kstat);
        self.stat_with_known_size(kstat)
    }
    /// 读取目录项内容。
    ///
    /// `off` 是 lwext4 目录读取 cookie，不一定等价于普通字节偏移。
    fn read_dentry(&self, off: usize, len: usize) -> SysResult<(Vec<u8>, isize)> {
        // `read_dir_from` is the only lwext4 operation here.  Keep directory
        // entry serialization and mount-table inspection outside the global
        // guard so a large directory does not block unrelated file reads.
        let (path, entries) = {
            let _ext4 = EXT4_OP_LOCK.lock();
            let inner = self.inner.get_unchecked_mut();
            let path = Self::live_path(inner);
            let entries = inner.f.read_dir_from(off as u64).map_err(SysErrNo::from)?;
            (path, entries)
        };
        let mut de: Vec<u8> = Vec::new();
        let (mut res, mut f_off) = (0usize, off);
        for entry in entries {
            if res + entry.len() > len {
                if res == 0 {
                    return Err(SysErrNo::EINVAL);
                }
                break;
            }
            res += entry.len();
            f_off = entry.off();
            de.extend_from_slice(entry.as_bytes());
        }
        // Update dir atime unless MS_NOATIME or MS_NODIRATIME suppresses it.
        if res > 0 {
            let suppress = MNT_TABLE
                .lock()
                .mount_for_path(&path)
                .map(|(_, _, _, flags)| {
                    flags.intersects(MountFlags::NOATIME | MountFlags::NODIRATIME)
                })
                .unwrap_or(false);
            if !suppress {
                let now = crate::timer::realtime();
                let _ = self.set_timestamps(Some(now.tv_sec as u64), None, None);
            }
        }
        // assert!(res != 0);
        Ok((de, f_off as isize))
    }

    /// 判断目录是否为空。
    ///
    /// 只要出现除 `.` 和 `..` 之外的目录项，就认为目录非空。
    fn is_dir_empty(&self) -> Result<bool, SysErrNo> {
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        if as_inode_type(file.file_type()) != InodeType::Dir {
            return Err(SysErrNo::ENOTDIR);
        }

        let entries = file.read_dir_from(0).map_err(SysErrNo::from)?;
        for entry in entries {
            let name_end = entry
                .d_name
                .iter()
                .position(|ch| *ch == 0)
                .unwrap_or(entry.d_name.len());
            let name = &entry.d_name[..name_end];
            if name != b"." && name != b".." {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// 读取符号链接目标路径。
    fn read_link(&self, buf: &mut [u8], bufsize: usize) -> SysResult<usize> {
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_readlink(buf, bufsize).map_err(SysErrNo::from)
    }

    /// 创建符号链接。
    fn sym_link(&self, target: &str, path: &str) -> SyscallRet {
        let _ext4 = EXT4_OP_LOCK.lock();
        let file = &mut self.inner.get_unchecked_mut().f;
        file.file_fsymlink(target, path).map_err(SysErrNo::from)
    }
    /// 获取硬链接计数。
    ///
    /// lwext4 在路径已不存在时可能返回 `ENOENT`，这里按 0 个 link 兼容延迟删除路径。
    fn link_cnt(&self) -> SyscallRet {
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        let r = file.links_cnt();
        if let Err(e) = r {
            if e == 2 {
                return Ok(0);
            } else {
                return Err(SysErrNo::from(e));
            }
        }
        Ok(r.unwrap() as usize)
    }

    /// 删除指定路径的目录项。
    ///
    /// 目录走 `dir_rm()`，普通文件和其他文件类型走 `file_remove()`。
    fn unlink(&self, path: &str) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let is_dir = as_inode_type(inner.f.types()) == InodeType::Dir;
        let file = &mut inner.f;
        let ret = if is_dir {
            file.dir_rm(path).map_err(SysErrNo::from)
        } else {
            file.file_remove(path).map_err(SysErrNo::from)?;
            MNT_TABLE.lock().remove_file(path);
            Ok(0)
        };
        if ret.is_ok() {
            self.invalidate_cached_stat();
        }
        ret
    }

    /// 返回当前可用于 lwext4 path-based API 的路径。
    fn path(&self) -> String {
        self.cached_path()
    }

    fn page_cache_path(&self) -> Option<Arc<str>> {
        Some(self.cached_page_cache_path())
    }

    fn cache_identity(&self) -> Option<(usize, usize)> {
        self.inode_identity
    }

    /// 从 VFS inode cache 记录新的路径别名。
    fn cache_path_alias(&self, path: &str) {
        self.add_alias_path(path);
    }

    /// 标记为延迟删除。
    ///
    /// 当文件已经 unlink 但仍有 fd 持有 inode 时，先标记延迟删除，等最后一个
    /// `Arc<Ext4Inode>` drop 时再真正移除磁盘文件。
    fn delay(&self) {
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        self.inner.get_unchecked_mut().delay = true;
        self.delayed.store(true, Ordering::Release);
    }

    /// 读取文件 mode bits。
    ///
    /// 当前路径失败时会尝试从 alias 恢复，兼容 rename/hard link 后的已打开 fd。
    fn fmode(&self) -> Result<u32, SysErrNo> {
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        match inner.f.file_mode() {
            Ok(mode) => Ok(mode),
            Err(_) => {
                let _ = self.recover_live_path(inner);
                inner.f.file_mode().map_err(SysErrNo::from)
            }
        }
    }
    /// 设置文件 mode bits。
    ///
    /// 如果传入 mode 未带文件类型位，则沿用当前 inode 类型，避免 chmod 类操作把
    /// regular/dir/symlink 类型位清掉。
    fn fmode_set(&self, mode: u32) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let mode_type = mode & 0o170000;
        let mode_type = if mode_type != 0 {
            mode_type
        } else {
            as_inode_type(inner.f.file_type()).mode_bits()
        };
        let mode = mode_type | (mode & 0o7777);
        let ret = match inner.f.file_mode_set(mode) {
            Ok(ret) => Ok(ret),
            Err(_) => {
                let _ = self.recover_live_path(inner);
                inner.f.file_mode_set(mode).map_err(SysErrNo::from)
            }
        };
        if ret.is_ok() {
            self.invalidate_cached_stat();
        }
        ret
    }

    /// 设置 inode owner uid/gid。
    fn owner_set(&self, uid: u32, gid: u32) -> SyscallRet {
        // Keep owner updates in the filesystem layer so stat and permission checks agree.
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let ret = match inner.f.file_owner_set(uid, gid) {
            Ok(ret) => Ok(ret),
            Err(_) => {
                let _ = self.recover_live_path(inner);
                inner.f.file_owner_set(uid, gid).map_err(SysErrNo::from)
            }
        };
        if ret.is_ok() {
            self.invalidate_cached_stat();
        }
        ret
    }

    fn seek_data(&self, offset: usize) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open(&path, O_RDONLY).map_err(SysErrNo::from)?;
        // Opening first establishes the stable `(mountpoint, inode)` cache
        // key.  Disabling before that would only affect this descriptor and
        // let another alias recreate a byte-only cache for the sparse inode.
        file.disable_write_back_cache().map_err(SysErrNo::from)?;
        match file.file_seek_data(offset as u64) {
            Ok(pos) => Ok(pos as usize),
            Err(rc) => Err(SysErrNo::from(rc)),
        }
    }

    fn seek_hole(&self, offset: usize) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open(&path, O_RDONLY).map_err(SysErrNo::from)?;
        // See `seek_data()`: the cache policy is inode-wide, so the
        // descriptor must be open before deriving its key.
        file.disable_write_back_cache().map_err(SysErrNo::from)?;
        match file.file_seek_hole(offset as u64) {
            Ok(pos) => Ok(pos as usize),
            Err(rc) => Err(SysErrNo::from(rc)),
        }
    }
}

/// 当 `Ext4Inode` 生命周期结束时，确保关闭底层文件句柄。
impl Drop for Ext4Inode {
    fn drop(&mut self) {
        let _ext4 = EXT4_OP_LOCK.lock();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        // 如果标记了延时删除，则在关闭前移除文件。
        if inner.delay {
            debug!("Ext4Inode delays unlink {:?}", path);
            inner.f.file_remove(&path);
            MNT_TABLE.lock().remove_file(&path);
        }
        inner.f.file_close().expect("failed to close fd");
    }
}

// --- 类型转换辅助函数 ---

/// 将内核 VFS 的 [`InodeType`] 转换为 EXT4 磁盘目录项类型。
fn as_ext4_de_type(types: InodeType) -> InodeTypes {
    match types {
        InodeType::BlockDevice => InodeTypes::EXT4_DE_BLKDEV,
        InodeType::CharDevice => InodeTypes::EXT4_DE_CHRDEV,
        InodeType::Dir => InodeTypes::EXT4_DE_DIR,
        InodeType::Fifo => InodeTypes::EXT4_DE_FIFO,
        InodeType::File => InodeTypes::EXT4_DE_REG_FILE,
        InodeType::Socket => InodeTypes::EXT4_DE_SOCK,
        InodeType::SymLink => InodeTypes::EXT4_DE_SYMLINK,
        InodeType::Unknown => InodeTypes::EXT4_DE_UNKNOWN,
    }
}

/// 将 lwext4 返回的 inode/目录项类型转换为内核通用 [`InodeType`]。
fn as_inode_type(types: InodeTypes) -> InodeType {
    match types {
        InodeTypes::EXT4_INODE_MODE_FIFO | InodeTypes::EXT4_DE_FIFO => InodeType::Fifo,
        InodeTypes::EXT4_INODE_MODE_CHARDEV | InodeTypes::EXT4_DE_CHRDEV => InodeType::CharDevice,
        InodeTypes::EXT4_INODE_MODE_DIRECTORY | InodeTypes::EXT4_DE_DIR => InodeType::Dir,
        InodeTypes::EXT4_INODE_MODE_BLOCKDEV | InodeTypes::EXT4_DE_BLKDEV => InodeType::BlockDevice,
        InodeTypes::EXT4_INODE_MODE_FILE | InodeTypes::EXT4_DE_REG_FILE => InodeType::File,
        InodeTypes::EXT4_INODE_MODE_SOFTLINK | InodeTypes::EXT4_DE_SYMLINK => InodeType::SymLink,
        InodeTypes::EXT4_INODE_MODE_SOCKET | InodeTypes::EXT4_DE_SOCK => InodeType::Socket,
        _ => {
            warn!("unknown file type: {:?}", types);
            unreachable!()
        }
    }
}
/// 规范化相对符号链接路径。
///
/// `base` 是当前 symlink 所在路径，`rel` 是 symlink 中保存的相对目标。
/// 该 helper 处理 `.`、`..` 和空分量，并返回绝对路径。
fn join_path(base: &str, rel: &str) -> String {
    let mut comps = Vec::new();

    for part in base.split('/') {
        if !part.is_empty() {
            comps.push(part);
        }
    }

    // 去掉当前文件名
    comps.pop();

    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            x => comps.push(x),
        }
    }

    format!("/{}", comps.join("/"))
}
