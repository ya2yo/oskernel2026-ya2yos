//! EXT4 inode adapter.
//!
//! 本目录把 `lwext4_rust::Ext4File` 适配为 Ya2yOS VFS [`Inode`] trait。
//! lwext4 wrapper 主要暴露路径式 API，因此 `Ext4Inode` 除了保存底层
//! `Ext4File` 外，还维护同一 inode 的路径别名，用于 hard link / rename 后继续
//! 找到一个可用路径。
//!
//! - `mod.rs`：共享 inode 状态、路径和 stat cache，以及生命周期管理。
//! - `io.rs`：文件数据读取、写入、truncate、sync 和 sparse seek。
//! - `namespace.rs`：创建、链接、rename、unlink 和目录元数据 epoch。
//! - `lookup.rs`：路径查找和符号链接解析。
//! - `metadata.rs`：metadata、目录读取和权限/属主操作。
//! - `vfs.rs`：唯一的 [`Inode`] trait 实现；只负责转发到各职责模块。

use log::{debug, warn};
use lwext4_rust::{
    bindings::{ext4_inode_stat, O_CREAT, O_EXCL, O_RDONLY, O_RDWR, O_TRUNC, SEEK_SET},
    Ext4File, InodeTypes,
};

use super::TaskMutex;
use crate::utils::perf::Ext4FstatMissReason;
#[cfg(feature = "perf")]
use crate::utils::perf::{
    Ext4CreatePhase, Ext4CreatePhaseGuard, Ext4FstatColdInodeKind, Ext4FstatMissGuard,
    Ext4FstatPath, Ext4FstatPathGuard, Ext4FstatRecoveryGuard, Ext4FstatStageRecorder,
    Ext4InodePhaseGuard, Ext4MetadataPhase, Ext4NamespacePhase, Ext4RenamePhase,
};
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

use lwext4_rust::file::{
    discard_path_cache, read_cached_at, rename_path_cache, write_cached_at, OsDirent,
};

mod io;
mod lookup;
mod metadata;
mod namespace;
mod vfs;

/// 防止符号链接死循环的最大跳转次数。
const MAX_LOOPTIMES: usize = 5;
/// `find_from_cached_parent()` has already resolved every component preceding
/// the final name.  Use a private sentinel to skip the compatibility scan for
/// intermediate symlinks after that final lookup misses.
const SKIP_INTERMEDIATE_SYMLINK_RETRY: usize = usize::MAX;
const QUOTA_RESERVE_GRANULARITY: usize = 64 * 1024;
const UNKNOWN_FILE_SIZE: usize = usize::MAX;

/// Bump after a successful namespace operation that can retire an inode and
/// let lwext4 reuse its inode number.  The value is deliberately mount-wide:
/// an unrelated unlink only makes `FsIndex` fall back to its existing live
/// `fstat()` validation, never lets it accept a stale identity.
static EXT4_IDENTITY_EPOCH: AtomicUsize = AtomicUsize::new(1);

/// Bump after a successful namespace operation whose affected parent
/// directory is not available as a live `Ext4Inode` here.  Directory-local
/// metadata changes use each inode's `directory_stat_epoch` instead, so
/// unrelated creates/chmods do not discard every cached directory stat on the
/// mount.  This must stay separate from `EXT4_IDENTITY_EPOCH`: unrelated
/// metadata changes do not make an inode identity unsafe.
static EXT4_DIRECTORY_STAT_EPOCH: AtomicUsize = AtomicUsize::new(1);

/// EXT4 inode 的 VFS 包装。
///
/// `Ext4Inode` 是 VFS 层看到的 inode 对象，内部用 `Ext4File` 调用 lwext4。
/// 由于底层接口以 path 为主，不是以稳定 inode handle 为主，因此这里还保存路径
/// alias，并在必要时重新打开到仍然存在的路径。
pub struct Ext4Inode {
    inner: SyncUnsafeCell<Ext4InodeInner>,
    /// Serializes access to the mutable `Ext4File` descriptor and alias list.
    /// It stays held while one inode crosses multiple separately serialized
    /// lwext4 calls, preventing another VFS operation from changing this
    /// descriptor between `open` and the actual I/O.
    io_state: TaskMutex,
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
    /// value without entering the lwext4 resource-lock path.
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
    /// Snapshot of [`EXT4_IDENTITY_EPOCH`] taken with `inode_identity` during
    /// construction.  Equality proves no inode-recycling namespace boundary
    /// has occurred since this VFS wrapper was created.
    identity_epoch: usize,
    /// Whether this VFS wrapper was constructed from the `ext4_stat_get()`
    /// result of a pathname lookup. Kept only for perf attribution; it never
    /// affects cache eligibility or fstat semantics.
    #[cfg(feature = "perf")]
    has_lookup_stat: bool,
    /// Repeated `stat(2)` calls on an unchanged regular source or artifact
    /// otherwise serialize on lwext4's global path-based metadata lookup.
    /// It also contains a directory lookup snapshot retained until either
    /// this directory's local metadata epoch or the conservative global
    /// namespace epoch changes.
    stat_cache: RwLock<Ext4StatCache>,
    /// Directory metadata changes local to this inode should not invalidate
    /// cached stats for every other directory in the mount.  Cross-directory
    /// namespace operations still use the mount-wide epoch below.
    directory_stat_epoch: AtomicUsize,
}

/// The stat cache and, for perf builds, the operation which most recently
/// made it stale share the existing cache lock. This adds no lock acquisition
/// domain: readers still take the same `stat_cache` read lock and writers take
/// the same write lock as before.
struct Ext4StatCache {
    stat: Option<Kstat>,
    directory_lookup_stat: Option<Ext4DirectoryLookupStat>,
    #[cfg(feature = "perf")]
    miss_reason: Ext4FstatMissReason,
}

/// A directory `Kstat` converted from the `ext4_stat_get()` already performed
/// by pathname lookup or a later `fstat()`. The epoch makes it safe to retain
/// until the next directory metadata mutation.
struct Ext4DirectoryLookupStat {
    stat: Kstat,
    local_epoch: usize,
    global_epoch: usize,
}

/// `Ext4Inode` 的可变内部状态。
///
/// `SyncUnsafeCell` 包住该结构后，外部方法可以在 `&self` 下调用 lwext4 的可变接口。
/// 所有访问必须持有所属 [`Ext4Inode`] 的 `io_state`；它让同一 inode 的 descriptor
/// 状态在多个分段的 lwext4 调用之间保持稳定。
pub struct Ext4InodeInner {
    /// lwext4 wrapper 的文件/目录句柄。
    f: Ext4File,
    /// 指向同一 inode 的路径别名，用于 hard link / rename 后继续找到可用路径。
    aliases: Vec<String>,
    /// 延迟删除标志。如果为 true，在该 inode 被 Drop 时会从磁盘删除对应文件。
    delay: bool,
}

/// `find()` only needs lwext4 serialization for the metadata query itself.
/// Constructing the VFS wrapper allocates Rust-side state after that query so
/// parallel Cargo lookups can hand it over sooner.
enum Ext4FindResult {
    Dir {
        stat: ext4_inode_stat,
        identity_epoch: usize,
        directory_stat_epoch: usize,
    },
    File {
        stat: ext4_inode_stat,
        identity_epoch: usize,
    },
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
        Self::new_with_lookup_stat(path, types, None, None, None)
    }

    /// Build an inode from a lookup that has already performed
    /// `ext4_stat_get()`.  Reusing the metadata avoids a second serialized
    /// lookup when `FsIndex` records the inode identity and, for regular
    /// files, when the first page-cache access asks for the file size.
    fn new_with_stat(
        path: &str,
        types: InodeTypes,
        stat: ext4_inode_stat,
        identity_epoch: usize,
        directory_stat_epoch: Option<usize>,
    ) -> Self {
        Self::new_with_lookup_stat(
            path,
            types,
            Some(stat),
            Some(identity_epoch),
            directory_stat_epoch,
        )
    }

    fn new_with_lookup_stat(
        path: &str,
        types: InodeTypes,
        lookup_stat: Option<ext4_inode_stat>,
        lookup_identity_epoch: Option<usize>,
        lookup_directory_stat_epoch: Option<usize>,
    ) -> Self {
        let inode_type = as_inode_type(types.clone());
        #[cfg(feature = "perf")]
        let has_lookup_stat = lookup_stat.is_some();
        let inode_identity = lookup_stat
            .as_ref()
            .and_then(|stat| (stat.st_ino != 0).then_some((stat.st_dev, stat.st_ino)));
        // `find()` captures this alongside the `(st_dev, st_ino)` returned by
        // lwext4. A later namespace mutation therefore cannot make an old
        // lookup result appear current.
        let identity_epoch =
            lookup_identity_epoch.unwrap_or_else(|| EXT4_IDENTITY_EPOCH.load(Ordering::Acquire));
        let (known_size, cached_stat) = if inode_type == InodeType::File {
            match lookup_stat {
                Some(stat) => (stat.st_size as usize, Some(Self::kstat_from_ext4(stat))),
                None => (UNKNOWN_FILE_SIZE, None),
            }
        } else {
            (UNKNOWN_FILE_SIZE, None)
        };
        let global_directory_stat_epoch = EXT4_DIRECTORY_STAT_EPOCH.load(Ordering::Acquire);
        let directory_stat_epoch =
            lookup_directory_stat_epoch.unwrap_or(global_directory_stat_epoch);
        let directory_lookup_stat = match (inode_type, lookup_stat, lookup_directory_stat_epoch) {
            (InodeType::Dir, Some(stat), Some(epoch)) => Some(Ext4DirectoryLookupStat {
                stat: Self::kstat_from_ext4(stat),
                local_epoch: epoch,
                global_epoch: epoch,
            }),
            _ => None,
        };
        let stat_cache = Ext4StatCache {
            stat: cached_stat,
            directory_lookup_stat,
            #[cfg(feature = "perf")]
            miss_reason: Ext4FstatMissReason::ColdInode,
        };
        Ext4Inode {
            inode_type,
            io_state: TaskMutex::new(),
            write_state: TaskMutex::new(),
            path: RwLock::new(Arc::from(path)),
            known_size: AtomicUsize::new(known_size),
            quota_reserved: AtomicUsize::new(0),
            delayed: AtomicBool::new(false),
            inode_identity,
            identity_epoch,
            #[cfg(feature = "perf")]
            has_lookup_stat,
            stat_cache: RwLock::new(stat_cache),
            directory_stat_epoch: AtomicUsize::new(directory_stat_epoch),
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
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::Alias);
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
    fn advance_identity_epoch() {
        EXT4_IDENTITY_EPOCH.fetch_add(1, Ordering::AcqRel);
    }

    #[inline]
    fn advance_global_directory_stat_epoch() {
        EXT4_DIRECTORY_STAT_EPOCH.fetch_add(1, Ordering::AcqRel);
    }

    #[inline]
    fn advance_local_directory_stat_epoch(&self) {
        if self.inode_type == InodeType::Dir {
            self.directory_stat_epoch.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Invalidate the directory containing a namespace path when that parent
    /// is already represented by the VFS inode index.  Falling back to the
    /// mount-wide epoch preserves correctness for uncached parents and for
    /// paths whose parent cannot be resolved without another filesystem walk.
    #[inline]
    fn advance_directory_stat_epoch_for_path(path: &str) {
        let trimmed = path.trim_end_matches('/');
        let Some((parent, _)) = trimmed.rsplit_once('/') else {
            Self::advance_global_directory_stat_epoch();
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_fstat_directory_parent_global();
            return;
        };
        let parent = if parent.is_empty() { "/" } else { parent };
        if Self::mark_cached_directory_stat(parent) {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_fstat_directory_parent_local();
        } else {
            Self::advance_global_directory_stat_epoch();
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_fstat_directory_parent_global();
        }
    }

    /// Invalidate one cached directory without broadening the mount-wide
    /// epoch.  A missing or non-directory FsIndex entry is deliberately
    /// reported to the caller so it can retain the conservative fallback.
    #[inline]
    fn mark_cached_directory_stat(path: &str) -> bool {
        if let Some(inode) = FsIndex::find_inode_idx(path) {
            if inode.types() == InodeType::Dir {
                inode.mark_directory_stat_changed();
                return true;
            }
        }
        false
    }

    #[inline]
    fn mark_cached_directory_stat_inode(inode: &Arc<dyn Inode>) {
        if inode.types() == InodeType::Dir {
            inode.mark_directory_stat_changed();
        }
    }

    #[inline]
    fn parent_path(path: &str) -> Option<&str> {
        let trimmed = path.trim_end_matches('/');
        let (parent, _) = trimmed.rsplit_once('/')?;
        Some(if parent.is_empty() { "/" } else { parent })
    }

    #[inline]
    fn cached_stat(&self) -> Option<Kstat> {
        if self.inode_type == InodeType::File {
            self.stat_cache.read().stat
        } else {
            None
        }
    }

    /// Return a directory stat while the conservative metadata epoch is
    /// unchanged.  Path lookup and a successful `fstat()` both populate this
    /// cache, so repeated stats on a stable directory avoid the mount-wide
    /// lwext4 gate.  Sampling the epoch before taking the cache gives this
    /// lock-free path a linearization point without entering lwext4's gate;
    /// a concurrent later mutation may validly be observed afterward.
    #[inline]
    fn cached_directory_stat(&self) -> Option<Kstat> {
        if self.inode_type != InodeType::Dir {
            return None;
        }

        let current_local_epoch = self.directory_stat_epoch.load(Ordering::Acquire);
        let current_global_epoch = EXT4_DIRECTORY_STAT_EPOCH.load(Ordering::Acquire);
        let cache = self.stat_cache.read();
        let directory_stat = cache.directory_lookup_stat.as_ref()?;
        let local_miss = directory_stat.local_epoch != current_local_epoch;
        let global_miss = directory_stat.global_epoch != current_global_epoch;
        if !local_miss && !global_miss {
            Some(directory_stat.stat)
        } else {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_fstat_directory_stat_epoch_miss(
                local_miss,
                global_miss,
            );
            None
        }
    }

    #[inline]
    fn update_cached_directory_stat(&self, stat: Kstat) {
        if self.inode_type == InodeType::Dir {
            let local_epoch = self.directory_stat_epoch.load(Ordering::Acquire);
            let global_epoch = EXT4_DIRECTORY_STAT_EPOCH.load(Ordering::Acquire);
            self.stat_cache.write().directory_lookup_stat = Some(Ext4DirectoryLookupStat {
                stat,
                local_epoch,
                global_epoch,
            });
        }
    }

    #[inline]
    fn update_cached_stat(&self, stat: Kstat) {
        if self.inode_type == InodeType::File {
            self.stat_cache.write().stat = Some(stat);
        }
    }

    #[inline]
    fn invalidate_cached_stat(&self, reason: Ext4FstatMissReason) {
        if self.inode_type == InodeType::File {
            let mut cache = self.stat_cache.write();
            cache.stat = None;
            #[cfg(feature = "perf")]
            {
                cache.miss_reason = reason;
                crate::utils::perf::record_ext4_fstat_cache_invalidation(reason);
            }
            #[cfg(not(feature = "perf"))]
            let _ = reason;
        }
    }

    #[cfg(feature = "perf")]
    #[inline]
    fn stat_cache_miss_reason(&self) -> Ext4FstatMissReason {
        self.stat_cache.read().miss_reason
    }

    /// `FsIndex` calls this only for a new candidate constructed immediately
    /// after it actually reclaimed idle entries. Do not evict a populated
    /// cache merely for accounting; a later real miss will carry this reason.
    #[cfg(feature = "perf")]
    #[inline]
    fn mark_stat_cache_fsidx_rebuild(&self) {
        if self.inode_type == InodeType::File {
            let mut cache = self.stat_cache.write();
            if cache.stat.is_none() {
                cache.miss_reason = Ext4FstatMissReason::FsIndexRebuild;
            }
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
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::Recovery);
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

        let _io_state = self.io_state.lock();
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

/// 当 `Ext4Inode` 生命周期结束时，确保关闭底层文件句柄。
impl Drop for Ext4Inode {
    fn drop(&mut self) {
        let remove_quota_path = {
            let inner = self.inner.get_unchecked_mut();
            let path = Self::live_path(inner);
            // 如果标记了延时删除，则在关闭前移除文件。
            let remove_quota_path = if inner.delay {
                debug!("Ext4Inode delays unlink {:?}", path);
                let remove_result = inner.f.file_remove(&path);
                if remove_result.is_ok() {
                    Self::advance_identity_epoch();
                    Self::advance_directory_stat_epoch_for_path(&path);
                    Some(path)
                } else {
                    None
                }
            } else {
                None
            };
            inner.f.file_close().expect("failed to close fd");
            remove_quota_path
        };
        if let Some(path) = remove_quota_path {
            MNT_TABLE.lock().remove_file(&path);
        }
    }
}

// ============================ 类型转换辅助函数 ============================

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
