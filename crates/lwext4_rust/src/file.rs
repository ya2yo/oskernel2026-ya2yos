use core::{
    cell::UnsafeCell,
    ffi::{c_char, c_void},
    ops::{Deref, DerefMut},
};

use crate::bindings::*;
#[cfg(feature = "perf")]
use crate::perf::{
    self, DirectWriteReason, FileWritePath, FstatStageEvent, RenameWriteBackStageEvent,
};
use crate::perf::{
    FstatStageObserver, RenameWriteBackStageObserver, SparseWriteCacheEvictCause,
    SparseWriteFlushReason,
};

extern "C" {
    #[link_name = "ext4_fseek_data"]
    fn ext4_fseek_data_raw(file: *mut ext4_file, offset: u64, result: *mut u64) -> i32;
    #[link_name = "ext4_fseek_hole"]
    fn ext4_fseek_hole_raw(file: *mut ext4_file, offset: u64, result: *mut u64) -> i32;
    #[link_name = "ext4_mode_owner_set"]
    fn ext4_mode_owner_set_raw(path: *const c_char, mode: u32, uid: u32, gid: u32) -> i32;
    #[link_name = "ext4_fopen2_with_metadata"]
    fn ext4_fopen2_with_metadata_raw(
        file: *mut ext4_file,
        path: *const c_char,
        flags: i32,
        mode: u32,
        uid: u32,
        gid: u32,
        stat: *mut ext4_inode_stat,
    ) -> i32;
    #[link_name = "ext4_dir_mk_exclusive"]
    fn ext4_dir_mk_exclusive_raw(path: *const c_char) -> i32;
    #[link_name = "ext4_dir_mk_exclusive_with_metadata"]
    fn ext4_dir_mk_exclusive_with_metadata_raw(
        path: *const c_char,
        mode: u32,
        uid: u32,
        gid: u32,
        stat: *mut ext4_inode_stat,
    ) -> i32;
}
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::{ffi::CString, vec::Vec};
use spin::{Lazy, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

const PAGE_SIZE: usize = 4096;
pub const PAGE_MASK: usize = !0xfff;
// The image has a 32 MiB journal. One dense cache write is committed by
// lwext4 as a transaction, so bound each FIFO entry at 4 MiB and reserve
// enough journal space for descriptor/revoke blocks plus checkpoint
// transactions from other compiler jobs.
const MAX_CACHED_FILE_SIZE: usize = 4 * 0x10_0000; // 4 MiB

// Sparse files cannot use the byte-only whole-file cache because writing that
// cache back would allocate every hole. Still, compiler/linker output often
// arrives as many adjacent sub-page writes to one sparse inode. Keep only a
// bounded range set so those writes can be committed together without
// changing the inode's extent layout. Adjacent and overlapping writes are
// coalesced into one dirty range; disjoint ranges remain separate so holes are
// never materialized. The payload and global budgets bound memory/journal
// pressure, like ext4's extent and block-layer limits, rather than imposing an
// arbitrary number of sparse runs.
const MAX_SPARSE_WRITE_BUFFER_SIZE: usize = 512 * 1024;
const MAX_TOTAL_SPARSE_WRITE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Lock callbacks for one Rust-owned write-back cache entry.  The lock value
/// is the stable address of `VFileCacheLock`; a preemptible kernel may park a
/// waiter on that resource instead of executing `spin::RwLock`'s busy loop.
pub type VFileCacheLockHook =
    unsafe extern "C" fn(ctx: *mut c_void, lock: *mut c_void, write: bool);

/// Releases the host-side lock bookkeeping after a dynamic cache entry is
/// destroyed. Unlike lwext4's mount-owned locks, file-cache locks are created
/// and evicted continuously, so retaining their host records would leak one
/// scheduler lock per cached pathname.
pub type VFileCacheLockReleaseHook = unsafe extern "C" fn(ctx: *mut c_void, lock: *mut c_void);

#[derive(Clone, Copy)]
struct VFileCacheLockHooks {
    ctx: usize,
    lock: Option<VFileCacheLockHook>,
    unlock: Option<VFileCacheLockHook>,
    release: Option<VFileCacheLockReleaseHook>,
}

impl VFileCacheLockHooks {
    const fn empty() -> Self {
        Self {
            ctx: 0,
            lock: None,
            unlock: None,
            release: None,
        }
    }
}

static VFILE_CACHE_LOCK_HOOKS: Lazy<Mutex<VFileCacheLockHooks>> =
    Lazy::new(|| Mutex::new(VFileCacheLockHooks::empty()));

/// Install the host's task-aware locking callbacks before concurrent VFS
/// access begins.  Standalone lwext4 users intentionally retain the spin-lock
/// fallback when no paired hooks are supplied.
pub fn setup_vfile_cache_lock_hooks(
    ctx: *mut c_void,
    lock_hook: Option<VFileCacheLockHook>,
    unlock_hook: Option<VFileCacheLockHook>,
    release_hook: Option<VFileCacheLockReleaseHook>,
) {
    *VFILE_CACHE_LOCK_HOOKS.lock() = VFileCacheLockHooks {
        ctx: ctx as usize,
        lock: lock_hook,
        unlock: unlock_hook,
        release: release_hook,
    };
}

fn aligned_down(addr: usize) -> usize {
    addr & PAGE_MASK
}

/// Convert ext4 on-disk directory entry types to the Linux `getdents64`
/// `d_type` ABI.  The two enums use different numeric values.
#[inline]
fn ext4_dirent_type_to_linux_dtype(inode_type: u8) -> u8 {
    const DT_UNKNOWN: u8 = 0;
    const DT_FIFO: u8 = 1;
    const DT_CHR: u8 = 2;
    const DT_DIR: u8 = 4;
    const DT_BLK: u8 = 6;
    const DT_REG: u8 = 8;
    const DT_LNK: u8 = 10;
    const DT_SOCK: u8 = 12;

    match inode_type as u32 {
        EXT4_DE_UNKNOWN => DT_UNKNOWN,
        EXT4_DE_REG_FILE => DT_REG,
        EXT4_DE_DIR => DT_DIR,
        EXT4_DE_CHRDEV => DT_CHR,
        EXT4_DE_BLKDEV => DT_BLK,
        EXT4_DE_FIFO => DT_FIFO,
        EXT4_DE_SOCK => DT_SOCK,
        EXT4_DE_SYMLINK => DT_LNK,
        _ => DT_UNKNOWN,
    }
}

// Ext4File文件操作与block device设备解耦了
pub struct Ext4File {
    //file_desc_map: BTreeMap<CString, ext4_file>,
    file_desc: ext4_file,
    file_path: CString,

    this_type: InodeTypes,

    has_opened: bool,
    last_flags: u32,
    pending_mode: Option<u32>,
    // Large files bypass the whole-file write-back cache after one size probe.
    cache_too_large: bool,
    // Local fast-path mirror of the global per-inode policy. Whole-file cache
    // does not preserve allocation extents, so sparse files must write
    // directly to lwext4 once their layout can contain holes.
    cache_disabled: bool,
    // Delayed-unlink files remain active while their pathname is hidden from
    // new opens. Keep their cache out of the global FIFO until the last fd
    // closes, otherwise concurrent temporary writers continually rebuild it.
    cache_pinned: bool,
    // A logical loop-mount quota failure leaves the dirty byte cache visible
    // to readers, but the simplified ext4 model cannot persist it. Defer the
    // close-time write-back until the pathname is removed.
    defer_close_flush: bool,
    /// Perf-only path attribution consumed after a successful `file_write_at`.
    #[cfg(feature = "perf")]
    last_write_path: FileWritePath,
}

impl Ext4File {
    pub fn new(path: &str, types: InodeTypes) -> Self {
        Self {
            file_desc: ext4_file {
                mp: core::ptr::null_mut(),
                inode: 0,
                flags: 0,
                fsize: 0,
                fpos: 0,
            },
            file_path: CString::new(path).expect("CString::new Ext4File path failed"),
            this_type: types,
            has_opened: false,
            last_flags: 0,
            pending_mode: None,
            cache_too_large: false,
            cache_disabled: false,
            cache_pinned: false,
            defer_close_flush: false,
            #[cfg(feature = "perf")]
            last_write_path: FileWritePath::Direct,
        }
    }

    pub fn path(&self) -> CString {
        self.file_path.clone()
    }

    pub fn path_str(&self) -> &str {
        self.file_path.to_str().unwrap_or("")
    }

    pub fn types(&self) -> InodeTypes {
        self.this_type.clone()
    }

    fn xattr_path(path: &str) -> Result<CString, i32> {
        CString::new(path).map_err(|_| EINVAL as i32)
    }

    fn xattr_name(name: &[u8]) -> Result<CString, i32> {
        CString::new(name).map_err(|_| EINVAL as i32)
    }

    /// Set an xattr through lwext4's pathname API.  The caller has already
    /// selected a live VFS path, so a normal lookup reaches a symlink target
    /// while an `l*` syscall can retain the final symlink itself.
    pub fn xattr_set(&self, path: &str, name: &[u8], value: &[u8]) -> Result<usize, i32> {
        let path = Self::xattr_path(path)?;
        let name = Self::xattr_name(name)?;
        let value_ptr = if value.is_empty() {
            core::ptr::null()
        } else {
            value.as_ptr().cast::<c_void>()
        };
        let r = unsafe {
            ext4_setxattr(
                path.as_ptr(),
                name.as_ptr(),
                name.as_bytes().len(),
                value_ptr,
                value.len(),
            )
        };
        if r == EOK as i32 {
            Ok(0)
        } else {
            Err(r)
        }
    }

    /// Read an xattr, or query its length when `value` is empty.
    pub fn xattr_get(&self, path: &str, name: &[u8], value: &mut [u8]) -> Result<usize, i32> {
        let path = Self::xattr_path(path)?;
        let name = Self::xattr_name(name)?;
        let value_ptr = if value.is_empty() {
            core::ptr::null_mut()
        } else {
            value.as_mut_ptr().cast::<c_void>()
        };
        let mut value_len = 0usize;
        let r = unsafe {
            ext4_getxattr(
                path.as_ptr(),
                name.as_ptr(),
                name.as_bytes().len(),
                value_ptr,
                value.len(),
                &mut value_len,
            )
        };
        if r == EOK as i32 {
            Ok(value_len)
        } else {
            Err(r)
        }
    }

    /// Return the NUL-separated xattr name list, or its required length when
    /// `list` is empty.
    pub fn xattr_list(&self, path: &str, list: &mut [u8]) -> Result<usize, i32> {
        let path = Self::xattr_path(path)?;
        let list_ptr = if list.is_empty() {
            core::ptr::null_mut()
        } else {
            list.as_mut_ptr().cast::<c_char>()
        };
        let mut list_len = 0usize;
        let r = unsafe { ext4_listxattr(path.as_ptr(), list_ptr, list.len(), &mut list_len) };
        if r == EOK as i32 {
            Ok(list_len)
        } else {
            Err(r)
        }
    }

    pub fn xattr_remove(&self, path: &str, name: &[u8]) -> Result<usize, i32> {
        let path = Self::xattr_path(path)?;
        let name = Self::xattr_name(name)?;
        let r = unsafe { ext4_removexattr(path.as_ptr(), name.as_ptr(), name.as_bytes().len()) };
        if r == EOK as i32 {
            Ok(0)
        } else {
            Err(r)
        }
    }

    /// File open function.
    ///
    /// |---------------------------------------------------------------|
    /// |   r or rb                 O_RDONLY                            |
    /// |---------------------------------------------------------------|
    /// |   w or wb                 O_WRONLY|O_CREAT|O_TRUNC            |
    /// |---------------------------------------------------------------|
    /// |   a or ab                 O_WRONLY|O_CREAT|O_APPEND           |
    /// |---------------------------------------------------------------|
    /// |   r+ or rb+ or r+b        O_RDWR                              |
    /// |---------------------------------------------------------------|
    /// |   w+ or wb+ or w+b        O_RDWR|O_CREAT|O_TRUNC              |
    /// |---------------------------------------------------------------|
    /// |   a+ or ab+ or a+b        O_RDWR|O_CREAT|O_APPEND             |
    /// |---------------------------------------------------------------|
    pub fn file_open(&mut self, path: &str, flags: u32) -> Result<usize, i32> {
        self.file_open_inner(path, flags, true)
    }

    /// Create a new regular file while applying its final mode and owner in
    /// the same lwext4 transaction as inode allocation and directory linking.
    /// The caller must pass O_CREAT|O_EXCL so existing files are not modified.
    pub fn file_open_with_metadata(
        &mut self,
        path: &str,
        flags: u32,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<ext4_inode_stat, i32> {
        let c_path = CString::new(path).expect("CString::new failed").into_raw();
        let mut stat = ext4_inode_stat::default();
        let r = unsafe {
            ext4_fopen2_with_metadata_raw(
                &mut self.file_desc,
                c_path,
                flags as i32,
                mode,
                uid,
                gid,
                &mut stat,
            )
        };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            if r == EEXIST as i32 {
                debug!("ext4_fopen2_with_metadata: {}, rc = {}", path, r);
            } else {
                error!("ext4_fopen2_with_metadata: {}, rc = {}", path, r);
            }
            return Err(r);
        }

        self.has_opened = true;
        self.last_flags = flags;
        self.pending_mode = Some(mode);
        Ok(stat)
    }

    /// Open a descriptor for a read-only operation without populating the
    /// delayed whole-file write-back cache.  Compiler workloads usually read
    /// each source/artifact once, so eagerly mirroring every file doubles the
    /// I/O and copying cost without improving locality.
    pub fn file_open_read_only(&mut self, path: &str) -> Result<usize, i32> {
        // A writable descriptor is also valid for reads.  Keep it when the
        // VFS inode is shared by a writer and a reader: otherwise each read
        // changes O_RDWR -> O_RDONLY and the next write opens the pathname
        // again.  Those repeated lwext4 path walks run under the mount-wide
        // operation lock and are especially expensive for parallel Cargo.
        // Callers serialize descriptor position and all lwext4 access before
        // reaching this method, so retaining the descriptor does not relax
        // the wrapper's concurrency guarantees.
        if self.is_open_for_read(path) {
            return Ok(EOK as usize);
        }
        self.file_open_inner(path, O_RDONLY, false)
    }

    /// Check whether this descriptor can serve a read without another
    /// pathname lookup.  Callers that use it must already serialize access to
    /// this `Ext4File`; the check itself is entirely Rust-side and does not
    /// enter lwext4.
    #[inline]
    pub fn is_open_for_read(&self, path: &str) -> bool {
        self.has_opened && self.path_str() == path && matches!(self.last_flags, O_RDONLY | O_RDWR)
    }

    /// Check whether this descriptor can serve an `O_RDWR` write without a
    /// second pathname lookup. Callers must already serialize access to this
    /// `Ext4File`.
    #[inline]
    pub fn is_open_for_write(&self, path: &str) -> bool {
        self.has_opened && self.path_str() == path && self.last_flags == O_RDWR
    }

    fn file_open_inner(
        &mut self,
        path: &str,
        flags: u32,
        prepare_write_back_cache: bool,
    ) -> Result<usize, i32> {
        if self.has_opened && self.last_flags == flags && self.path_str() == path {
            // The common read path repeatedly opens the same VFS descriptor
            // with O_RDONLY. Reuse it without allocating comparison CStrings
            // while the global lwext4 lock is held.
            return Ok(EOK as usize);
        }
        // Sparse ranges are inode-keyed rather than descriptor-keyed.  Keep
        // them buffered across an O_RDWR -> O_RDONLY switch: a following read
        // overlays the dirty ranges in memory, while the later visibility or
        // durability barrier opens a temporary O_RDWR descriptor if this
        // active descriptor is no longer writable.  Flushing here used to
        // turn a cross-FD read into several small ext4 writes under the
        // mount-wide operation lock.
        // A caller that really reuses this wrapper for another pathname still
        // needs the old descriptor's bytes published before its inode key is
        // lost; normal Ext4Inode callers retain one pathname per wrapper.
        if self.file_desc.mp != core::ptr::null_mut() && self.path_str() != path {
            self.flush_sparse_write_buffer(SparseWriteFlushReason::Other)?;
        }
        let c_path = CString::new(path).expect("CString::new failed");

        //let to_map = c_path.clone();
        let c_path = c_path.into_raw();
        let r = if flags & O_EXCL != 0 {
            unsafe { ext4_fopen2(&mut self.file_desc, c_path, flags as i32) }
        } else {
            let c_flags = Self::flags_to_cstring(flags).into_raw();
            let r = unsafe { ext4_fopen(&mut self.file_desc, c_path, c_flags) };
            unsafe {
                drop(CString::from_raw(c_flags));
            }
            r
        };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_fopen: {}, rc = {}", path, r);
            return Err(r);
        }

        self.has_opened = true;
        self.last_flags = flags;
        if prepare_write_back_cache {
            self.cache_disabled = self.whole_file_cache_disabled();
            if flags & O_TRUNC != 0 {
                if let Some(key) = self.whole_file_cache_key() {
                    // `ext4_fopen(..., O_TRUNC)` has already discarded the
                    // on-disk contents.  Any byte-only mirror belongs to the old
                    // contents as well, so it must not be written back later.
                    discard_inode_caches(key);
                    self.discard_sparse_write_buffer();
                    clear_whole_file_cache_policy(key);
                } else {
                    discard_path_cache(path);
                }
                self.cache_disabled = false;
                self.cache_too_large = false;
            } else if !self.cache_disabled
                && self.this_type == InodeTypes::EXT4_DE_REG_FILE
                && ext4_file_has_hole(&mut self.file_desc)
            {
                // A byte-only cache cannot preserve existing hole extents.  This
                // must be established before the first ordinary read can build a
                // dense zero-filled mirror of a sparse inode.
                self.disable_write_back_cache()?;
            }
        }

        //self.file_desc_map.insert(to_map, fd); // store c_path
        //debug!("file_open {}, mp={:#x}", path, self.file_desc.mp as usize);
        Ok(EOK as usize)
    }

    /// Reuse the descriptor already associated with this pathname whenever
    /// possible; this is the hot path for VFS `write_at` calls.
    pub fn ensure_open(&mut self, flags: u32) -> Result<usize, i32> {
        if self.has_opened && self.last_flags == flags {
            return Ok(EOK as usize);
        }
        let path = String::from(self.path_str());
        self.file_open(&path, flags)
    }

    /// Keep a logical quota failure from flushing dirty byte-cache data when
    /// this descriptor closes.  The caller must arrange for the cache to be
    /// discarded with the pathname that can no longer be persisted.
    pub fn defer_close_flush(&mut self) {
        self.defer_close_flush = true;
    }

    pub fn file_close(&mut self) -> Result<usize, i32> {
        if self.file_desc.mp != core::ptr::null_mut() {
            if !self.defer_close_flush {
                //debug!("file_close {:?}", self.get_path());
                self.file_cache_flush_with_sparse_reason(SparseWriteFlushReason::Close)?;
            }
            unsafe {
                ext4_fclose(&mut self.file_desc);
            }
        }

        self.has_opened = false;
        self.defer_close_flush = false;

        Ok(0)
    }

    /// Close a descriptor without flushing the global block cache.
    ///
    /// Creation callers commonly perform several metadata operations as one
    /// transaction.  The ext4 descriptor still must be closed, but flushing
    /// after each empty file serializes and repeats the same cache write-back.
    /// The caller remains responsible for syncing when visibility or
    /// durability is required.
    pub fn file_close_without_cache_flush(&mut self) -> Result<usize, i32> {
        if self.file_desc.mp != core::ptr::null_mut() {
            // Delayed sparse ranges must reach lwext4 before their owning
            // descriptor disappears.  This deliberately does not call the
            // mount-wide block-cache flush used by `file_close()`.
            self.flush_sparse_write_buffer(SparseWriteFlushReason::Close)?;
            unsafe {
                ext4_fclose(&mut self.file_desc);
            }
        }

        self.has_opened = false;

        Ok(0)
    }

    pub fn flags_to_cstring(flags: u32) -> CString {
        let cstr = match flags {
            O_RDONLY => "rb",
            O_RDWR => "r+",
            0x241 => "wb", // O_WRONLY | O_CREAT | O_TRUNC
            0x441 => "ab", // O_WRONLY | O_CREAT | O_APPEND
            0x242 => "w+", // O_RDWR | O_CREAT | O_TRUNC
            0x442 => "a+", // O_RDWR | O_CREAT | O_APPEND
            _ => {
                warn!("Unknown File Open Flags: {:#x}", flags);
                "r+"
            }
        };
        //debug!("flags_to_cstring: {}", cstr);
        CString::new(cstr).expect("CString::new OpenFlags failed")
    }

    /// Inode types:
    /// EXT4_DIRENTRY_UNKNOWN
    /// EXT4_DE_REG_FILE
    /// EXT4_DE_DIR
    /// EXT4_DE_CHRDEV
    /// EXT4_DE_BLKDEV
    /// EXT4_DE_FIFO
    /// EXT4_DE_SOCK
    /// EXT4_DE_SYMLINK
    ///
    /// Check if inode exists.
    pub fn check_inode_exist(&mut self, path: &str, types: InodeTypes) -> bool {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        // let mtype = types.clone();
        let r = unsafe { ext4_inode_exist(c_path, types as i32) }; //eg: types: EXT4_DE_REG_FILE
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r == EOK as i32 {
            // debug!("{:?} {} Exist", mtype, path);
            true //Exist
        } else {
            // debug!("{:?} {} No Exist. ext4_inode_exist rc = {}", mtype, path, r);
            false
        }
    }

    /// Resolve a pathname once and return its inode type plus metadata.
    ///
    /// `ext4_stat_get()` accepts any final directory-entry type and returns
    /// the inode mode, unlike `ext4_inode_exist()` which needs one full path
    /// walk for every candidate type.  Callers that are about to cache the
    /// result can reuse the returned stat instead of immediately issuing a
    /// second `ext4_stat_get()` for the same path.
    pub fn inode_type_and_stat_at(&self, path: &str) -> Result<(InodeTypes, ext4_inode_stat), i32> {
        let c_path = CString::new(path).expect("CString::new failed").into_raw();
        let mut stat = ext4_inode_stat::default();
        let r = unsafe { ext4_stat_get(c_path, &mut stat) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            return Err(r);
        }
        overlay_cached_stat(path, &mut stat);
        Ok((InodeTypes::from((stat.st_mode as usize) & 0xf000), stat))
    }

    /// Resolve a pathname once and return its inode type.
    ///
    /// Prefer [`Self::inode_type_and_stat_at`] when the caller will also need
    /// the metadata for an inode-cache insertion.
    pub fn inode_type_at(&self, path: &str) -> Result<InodeTypes, i32> {
        self.inode_type_and_stat_at(path)
            .map(|(inode_type, _)| inode_type)
    }

    pub fn file_readlink(&mut self, buf: &mut [u8], bufsize: usize) -> Result<usize, i32> {
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let buf_c_char = buf.as_mut_ptr() as *mut c_char;
        let mut rcnt = 0usize;
        let r = unsafe { ext4_readlink(c_path, buf_c_char, bufsize, &mut rcnt) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_readlink error: rc = {}", r);
            return Err(r);
        }
        Ok(rcnt)
    }

    pub fn is_symlink(&mut self, path: &str) -> bool {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let mut buf = [0 as c_char; 1];
        let mut rcnt = 0usize;
        let r = unsafe { ext4_readlink(c_path, buf.as_mut_ptr(), buf.len(), &mut rcnt) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        r == EOK as i32
    }

    //ext4_fsymlink(const char *target, const char *path)
    pub fn file_fsymlink(&mut self, target: &str, path: &str) -> Result<usize, i32> {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let target_path = CString::new(target).expect("CString::new failed");
        let target_path = target_path.into_raw();
        let r = unsafe { ext4_fsymlink(target_path, c_path) };
        if r != EOK as i32 {
            error!("ext4_fsymlink error: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Rename file and directory
    pub fn file_rename(&mut self, path: &str, new_path: &str) -> Result<usize, i32> {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let c_new_path = CString::new(new_path).expect("CString::new failed");
        let c_new_path = c_new_path.into_raw();
        let r = unsafe { ext4_frename(c_path, c_new_path) };
        unsafe {
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_new_path));
        }
        if r != EOK as i32 {
            error!("ext4_frename error: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Create a hard link for a file.
    /// - `path`: Path to existing file.
    /// - `hardlink_path`: Path of the new hard link (must not exist).
    pub fn file_hardlink(&mut self, path: &str, hardlink_path: &str) -> Result<usize, i32> {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let c_hardlink_path = CString::new(hardlink_path).expect("CString::new failed");
        let c_hardlink_path = c_hardlink_path.into_raw();
        let r = unsafe { ext4_flink(c_path, c_hardlink_path) };
        unsafe {
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_hardlink_path));
        }
        if r != EOK as i32 {
            error!("ext4_flink error: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Remove file by path.
    pub fn file_remove(&mut self, path: &str) -> Result<usize, i32> {
        //debug!("file_remove {}", path);
        let cache_path = String::from(path);
        let target_is_open_inode = self.file_path.to_str().ok() == Some(path);
        let cache_key = if target_is_open_inode {
            self.whole_file_cache_key()
                .or_else(|| whole_file_cache_key_for_path(path))
        } else {
            whole_file_cache_key_for_path(path)
        };
        let removes_last_link = if cache_key.is_some() {
            links_cnt_for_path(path)
                .map(|count| count == 1)
                .unwrap_or(false)
        } else {
            false
        };

        // A last-link unlink makes the file contents unreachable immediately;
        // flushing a large dirty whole-file cache before removing that inode
        // only burns filesystem bandwidth. Preserve the cache on every
        // failure path, but discard it after the directory entry is removed.
        if let Some(key) = cache_key {
            if !removes_last_link {
                // In the normal unlink path `self` is the target inode, so
                // this commits its range buffer before another hard link
                // exposes the same inode.  The descriptor-key check inside
                // the helper makes parent-directory callers a no-op.
                self.flush_sparse_write_buffer(SparseWriteFlushReason::Other)?;
                flush_inode_caches(key)?;
            }
        } else if if_cache(cache_path.clone()) {
            write_back_cache(cache_path.clone())?;
        }
        if !removes_last_link {
            flush_ext4_block_cache_for_path(path)?;
        }

        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();

        let r = unsafe { ext4_fremove(c_path) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if (r != EOK as i32) && (r != ENOENT as i32) {
            error!("ext4_fremove error: rc = {}", r);
            return Err(r);
        }

        // The directory entry is now gone (or was already absent), so its
        // path-keyed cache cannot be reused.  Preserve it on every real
        // ext4_fremove failure above.
        discard_path_cache(&cache_path);
        if r == EOK as i32 && removes_last_link {
            if let Some(key) = cache_key {
                SPARSE_WRITE_BUFFERS.lock().remove(&key);
                discard_inode_caches(key);
                clear_whole_file_cache_policy(key);
            }
        }

        self.has_opened = false;
        self.cache_too_large = false;
        self.cache_disabled = r != EOK as i32 || !removes_last_link;
        Ok(EOK as usize)
    }

    // 检查是否值得建立文件缓存。大文件直接走 ext4，避免一次性占用大量堆。
    fn check_cached(&mut self, file_path: String) -> Result<(), i32> {
        // These files are regenerated and synchronously synced on every procfs
        // refresh. Keeping them in the global delayed write-back FIFO leaves
        // stale entries after process teardown and can recurse into lwext4
        // while an ext4 operation is already in progress.
        if is_proc_task_runtime_file(&file_path) {
            return Ok(());
        }

        if !self.write_back_cache_enabled(&file_path) {
            return Ok(());
        }

        if if_cache(file_path.clone()) {
            return Ok(());
        }

        if self.cache_too_large {
            return Ok(());
        }

        let c_path = CString::new(file_path.as_str()).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let c_flags = Ext4File::flags_to_cstring(O_RDONLY).into_raw();
        let mut cache_desc = ext4_file {
            mp: core::ptr::null_mut(),
            inode: 0,
            flags: 0,
            fsize: 0,
            fpos: 0,
        };

        // Query cache contents through a separate descriptor.  Reopening
        // self.file_desc here can otherwise desynchronize an active file from
        // its path/cache key when lwext4 rejects the cache open.
        let r = unsafe { ext4_fopen(&mut cache_desc, c_path, c_flags) };
        unsafe {
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_flags));
        }
        if r != EOK as i32 {
            error!("check_cached ext4_fopen: {}, rc = {}", file_path, r);
            return Ok(());
        }

        let size = unsafe { ext4_fsize(&mut cache_desc) as usize };
        if size > MAX_CACHED_FILE_SIZE {
            unsafe {
                ext4_fclose(&mut cache_desc);
            }
            self.cache_too_large = true;
            return Ok(());
        }

        if self.this_type == InodeTypes::EXT4_DE_REG_FILE && ext4_file_has_hole(&mut cache_desc) {
            unsafe {
                ext4_fclose(&mut cache_desc);
            }
            // `file_open()` normally detects this first.  Keep this second
            // check beside cache creation as a defensive barrier for an
            // existing sparse inode and never mirror its holes as bytes.
            self.disable_write_back_cache()?;
            return Ok(());
        }

        let cache = Arc::new(VFileCacheLock::new(VFileCache::new()));
        let mut cache_writer = cache.write();
        cache_writer.mode = self.pending_mode;
        cache_writer.inode_key = whole_file_cache_key_for_desc(&cache_desc);
        let aligned_size = aligned_down(size) + PAGE_SIZE;
        cache_writer.data = Vec::new();
        if cache_writer.data.try_reserve_exact(aligned_size).is_err() {
            unsafe {
                ext4_fclose(&mut cache_desc);
            }
            return Ok(());
        }
        let data = &mut cache_writer.data;
        unsafe {
            data.set_len(aligned_size);
        }
        cache_writer.size = size;
        if size == 0 {
            unsafe {
                ext4_fclose(&mut cache_desc);
            }
            drop(cache_writer);
            if self.cache_pinned {
                insert_cache(file_path.clone(), &cache);
                #[cfg(feature = "perf")]
                perf::record_write_cache_init(0);
                debug!("initialize pinned cache! {}", file_path);
            } else if insert_fifo(file_path.clone()).is_ok() {
                insert_cache(file_path.clone(), &cache);
                #[cfg(feature = "perf")]
                perf::record_write_cache_init(0);
                debug!("initialize cache! {}", file_path);
            }
            return Ok(());
        }
        unsafe { ext4_fseek(&mut cache_desc, 0, SEEK_SET) };
        let mut rw_count = 0;
        let r = unsafe {
            ext4_fread(
                &mut cache_desc,
                cache_writer.data.as_mut_ptr() as _,
                size,
                &mut rw_count,
            )
        };
        unsafe {
            ext4_fclose(&mut cache_desc);
        }
        if r != EOK as i32 || rw_count != size {
            drop(cache_writer);
            error!(
                "check_cached ext4_fread: {}, rc = {}, expected {}, got {}",
                file_path, r, size, rw_count
            );
            return Ok(());
        }
        drop(cache_writer);
        if self.cache_pinned {
            insert_cache(file_path.clone(), &cache);
            #[cfg(feature = "perf")]
            perf::record_write_cache_init(size);
            debug!("initialize pinned cache! {}", file_path);
        } else if insert_fifo(file_path.clone()).is_ok() {
            insert_cache(file_path.clone(), &cache);
            #[cfg(feature = "perf")]
            perf::record_write_cache_init(size);
            debug!("initialize cache! {}", file_path);
        }
        Ok(())
    }

    pub fn file_seek(&mut self, offset: i64, seek_type: u32) -> Result<usize, i32> {
        if self.this_type != InodeTypes::EXT4_DE_DIR {
            let path = String::from((*self.file_path).to_str().unwrap());
            let cache_enabled = self.write_back_cache_enabled(&path);
            if cache_enabled && !self.cache_too_large {
                self.check_cached(path.clone())?;
            }

            if cache_enabled {
                if let Some(cache) = get_cache(&path) {
                    let mut cache_writer = cache.write();
                    if !cache_writer.evicting {
                        cache_writer.offset =
                            seek_pos(cache_writer.offset, cache_writer.size, offset, seek_type)?;
                        return Ok(EOK as usize);
                    }
                }
            }
        }

        self.file_desc.fpos = seek_pos(
            self.file_desc.fpos as usize,
            self.file_desc.fsize as usize,
            offset,
            seek_type,
        )? as u64;

        Ok(EOK as usize)
    }

    pub fn file_read(&mut self, buff: &mut [u8]) -> Result<usize, i32> {
        let path = String::from((*self.file_path).to_str().unwrap());
        if self.write_back_cache_enabled(&path) {
            if let Some(cache) = get_cache(&path) {
                //找到cache直接读cache
                let cache_read = cache.read();
                if !cache_read.evicting {
                    let data = cache_read.get_data_slice();
                    if cache_read.offset >= cache_read.size {
                        return Ok(0);
                    }
                    let length = buff.len();
                    let end = cache_read
                        .offset
                        .checked_add(length)
                        .ok_or(EINVAL as i32)?
                        .min(cache_read.size);
                    let r_sz = end - cache_read.offset;
                    //debug!("data.len={:x},end={:x}", data.len(), end);
                    if length <= 10 {
                        for i in 0..r_sz {
                            buff[i] = data[cache_read.offset + i];
                        }
                    } else {
                        buff[..r_sz].copy_from_slice(&data[cache_read.offset..end]);
                    }

                    /*
                    debug!(
                        "file_read {},len = {:x},offset is {:x}",
                        path, r_sz, cache_read.offset
                    );
                    */

                    return Ok(r_sz);
                }
            }
        }

        let offset = self.file_desc.fpos as usize;
        if let Some(read_size) = self.read_sparse_write_buffer_at(offset, buff)? {
            return Ok(read_size);
        }
        self.flush_sparse_write_buffer(SparseWriteFlushReason::Other)?;

        let mut rw_count = 0;
        let r = unsafe {
            ext4_fread(
                &mut self.file_desc,
                buff.as_mut_ptr() as _,
                buff.len(),
                &mut rw_count,
            )
        };

        if r != EOK as i32 {
            error!("ext4_fread: rc = {}", r);
            return Err(r);
        }

        //debug!("file_read {:?}, len={}", self.get_path(), rw_count);
        Ok(rw_count)
    }

    /// Read from an already-open descriptor at an explicit offset, bypassing
    /// the whole-file cache.  The VFS keeps the userspace file offset itself;
    /// setting `fpos` here avoids an extra `ext4_fseek` for every read syscall.
    pub fn file_read_at(&mut self, offset: usize, buff: &mut [u8]) -> Result<usize, i32> {
        if let Some(read_size) = self.read_sparse_write_buffer_at(offset, buff)? {
            return Ok(read_size);
        }
        // No dirty sparse range overlaps this inode.  The normal direct read
        // path remains unchanged in that case.
        self.flush_sparse_write_buffer(SparseWriteFlushReason::Other)?;
        self.file_desc.fpos = offset as u64;
        let mut rw_count = 0;
        let r = unsafe {
            ext4_fread(
                &mut self.file_desc,
                buff.as_mut_ptr() as _,
                buff.len(),
                &mut rw_count,
            )
        };
        if r != EOK as i32 {
            error!("ext4_fread: rc = {}", r);
            return Err(r);
        }
        Ok(rw_count)
    }

    /// Whether this regular inode currently bypasses the dense whole-file
    /// cache because its allocation map can contain holes.
    #[inline]
    fn sparse_write_buffer_enabled(&self) -> bool {
        self.this_type == InodeTypes::EXT4_DE_REG_FILE
            && (self.cache_disabled || self.whole_file_cache_disabled())
    }

    /// Return the buffered logical EOF for this inode, if one exists.
    fn sparse_write_buffer_end(&self) -> Option<usize> {
        let key = self.whole_file_cache_key()?;
        let buffers = SPARSE_WRITE_BUFFERS.lock();
        buffers
            .entries
            .get(&key)?
            .runs
            .iter()
            .filter_map(|buffer| buffer.offset.checked_add(buffer.data.len()))
            .max()
    }

    /// Read the on-disk bytes and then overlay pending sparse dirty ranges.
    ///
    /// A sparse range is keyed by inode and is visible to every open file
    /// description.  Reading it does not require committing it to lwext4:
    /// bytes in an existing on-disk extent come from `ext4_fread`, holes after
    /// the on-disk EOF are zero-filled, and dirty intersections overwrite
    /// both.  Returning `None` means that this inode has no delayed sparse
    /// range and the caller must retain its ordinary read path.
    fn read_sparse_write_buffer_at(
        &mut self,
        offset: usize,
        buff: &mut [u8],
    ) -> Result<Option<usize>, i32> {
        if buff.is_empty() {
            return Ok(Some(0));
        }
        let Some(key) = self.whole_file_cache_key() else {
            return Ok(None);
        };
        let sparse_end = {
            let buffers = SPARSE_WRITE_BUFFERS.lock();
            let Some(buffer_set) = buffers.entries.get(&key) else {
                return Ok(None);
            };
            buffer_set
                .runs
                .iter()
                .filter_map(|buffer| buffer.offset.checked_add(buffer.data.len()))
                .max()
                .unwrap_or(0)
        };
        let request_end = offset.checked_add(buff.len()).ok_or(EFBIG as i32)?;

        self.file_desc.fpos = offset as u64;
        let mut rw_count = 0;
        let r = unsafe {
            ext4_fread(
                &mut self.file_desc,
                buff.as_mut_ptr() as _,
                buff.len(),
                &mut rw_count,
            )
        };
        if r != EOK as i32 {
            error!("ext4_fread: rc = {}", r);
            return Err(r);
        }

        let sparse_visible_len = sparse_end.saturating_sub(offset).min(buff.len());
        let visible_len = rw_count.max(sparse_visible_len);
        if rw_count < visible_len {
            buff[rw_count..visible_len].fill(0);
        }

        #[cfg(feature = "perf")]
        let mut dirty_bytes = 0;
        {
            let buffers = SPARSE_WRITE_BUFFERS.lock();
            if let Some(buffer_set) = buffers.entries.get(&key) {
                // Connected writes are compacted at insertion. Replaying the
                // retained order preserves last-write-wins for any overlapping
                // legacy range without materialising holes in lwext4.
                for buffer in &buffer_set.runs {
                    let Some(buffer_end) = buffer.offset.checked_add(buffer.data.len()) else {
                        continue;
                    };
                    let copy_start = buffer.offset.max(offset);
                    let copy_end = buffer_end.min(request_end);
                    if copy_start >= copy_end {
                        continue;
                    }
                    let source_start = copy_start - buffer.offset;
                    let copy_len = copy_end - copy_start;
                    let destination_start = copy_start - offset;
                    buff[destination_start..destination_start + copy_len]
                        .copy_from_slice(&buffer.data[source_start..source_start + copy_len]);
                    #[cfg(feature = "perf")]
                    {
                        dirty_bytes += copy_len;
                    }
                }
            }
        }
        self.file_desc.fpos = offset.saturating_add(visible_len) as u64;
        #[cfg(feature = "perf")]
        perf::record_sparse_read_overlay(visible_len, dirty_bytes);
        Ok(Some(visible_len))
    }

    /// Drop pending bytes after the caller has made the inode unreachable or
    /// replaced its contents with `O_TRUNC`.  This differs from write-back:
    /// there is intentionally no destination left to preserve.
    fn discard_sparse_write_buffer(&self) {
        if let Some(key) = self.whole_file_cache_key() {
            SPARSE_WRITE_BUFFERS.lock().remove(&key);
        }
    }

    /// Commit this inode's bounded sparse-write run.  The caller already
    /// holds Ya2yOS's global lwext4 guard, so the descriptor and the
    /// inode-keyed buffer remain serialized with other filesystem operations.
    fn flush_sparse_write_buffer(&mut self, reason: SparseWriteFlushReason) -> Result<usize, i32> {
        self.flush_sparse_write_buffer_with_cause(reason, None)
    }

    /// Publish pending sparse ranges before a successful rename. Dense
    /// byte-cache state is handled by the caller after the directory entry
    /// move succeeds.
    pub fn flush_sparse_write_buffer_for_rename(&mut self) -> Result<usize, i32> {
        self.flush_sparse_write_buffer(SparseWriteFlushReason::Rename)
    }

    fn flush_sparse_write_buffer_with_cache_evict_cause(
        &mut self,
        cause: SparseWriteCacheEvictCause,
    ) -> Result<usize, i32> {
        self.flush_sparse_write_buffer_with_cause(SparseWriteFlushReason::CacheEvict, Some(cause))
    }

    fn flush_sparse_write_buffer_with_cause(
        &mut self,
        reason: SparseWriteFlushReason,
        cache_evict_cause: Option<SparseWriteCacheEvictCause>,
    ) -> Result<usize, i32> {
        let Some(key) = self.whole_file_cache_key() else {
            return Ok(0);
        };
        let Some(mut buffers) = SPARSE_WRITE_BUFFERS.lock().remove(&key) else {
            return Ok(0);
        };
        // Linux writeback walks the page cache in file-offset order and lets
        // ext4 merge adjacent blocks into one bio. Keep the disjoint sparse
        // ranges ordered before issuing their individual writes; holes remain
        // separate and are never represented by zero-filled bytes.
        buffers
            .runs
            .make_contiguous()
            .sort_unstable_by_key(|buffer| buffer.offset);
        #[cfg(feature = "perf")]
        {
            perf::record_sparse_write_flush_trigger(reason);
            perf::record_sparse_write_flush_batch(
                cache_evict_cause,
                buffers.runs.len(),
                buffers.bytes,
            );
        }
        #[cfg(not(feature = "perf"))]
        let _ = reason;
        #[cfg(not(feature = "perf"))]
        let _ = cache_evict_cause;

        // Prefer the active writable descriptor so delayed-unlink files keep
        // working after their pathname disappears.  A reader may instead be
        // holding an O_RDONLY descriptor while another FD owns the range;
        // flush through a temporary writable descriptor in that case.
        let use_active_descriptor = self.file_desc.mp != core::ptr::null_mut()
            && self.has_opened
            && self.last_flags == O_RDWR;
        let mut temporary_desc = ext4_file {
            mp: core::ptr::null_mut(),
            inode: 0,
            flags: 0,
            fsize: 0,
            fpos: 0,
        };
        if !use_active_descriptor {
            let c_path = self.file_path.clone().into_raw();
            let c_flags = Self::flags_to_cstring(O_RDWR).into_raw();
            let open_r = unsafe { ext4_fopen(&mut temporary_desc, c_path, c_flags) };
            unsafe {
                drop(CString::from_raw(c_path));
                drop(CString::from_raw(c_flags));
            }
            if open_r != EOK as i32 {
                SPARSE_WRITE_BUFFERS.lock().insert(key, buffers);
                error!("sparse write buffer ext4_fopen: rc = {}", open_r);
                return Err(open_r);
            }
        }

        let mut largest_end = self.file_desc.fsize;
        let file_desc = if use_active_descriptor {
            &mut self.file_desc
        } else {
            &mut temporary_desc
        };
        // `file_write_at()` owns the descriptor position for its duration.
        // Restoring it is necessary for `file_write()` and sequential readers
        // that may flush several runs as a visibility barrier.
        let saved_pos = file_desc.fpos;
        while let Some(buffer) = buffers.runs.pop_front() {
            let Some(end) = buffer.offset.checked_add(buffer.data.len()) else {
                buffers.runs.push_front(buffer);
                SPARSE_WRITE_BUFFERS.lock().insert(key, buffers);
                file_desc.fpos = saved_pos;
                if !use_active_descriptor {
                    unsafe { ext4_fclose(&mut temporary_desc) };
                }
                return Err(EFBIG as i32);
            };
            let expected = buffer.data.len();
            buffers.bytes = buffers.bytes.saturating_sub(expected);
            file_desc.fpos = buffer.offset as u64;
            let mut rw_count = 0;
            let r = unsafe {
                ext4_fwrite(
                    file_desc,
                    buffer.data.as_ptr() as _,
                    expected,
                    &mut rw_count,
                )
            };
            if r != EOK as i32 || rw_count != expected {
                buffers.bytes = buffers.bytes.saturating_add(expected);
                buffers.runs.push_front(buffer);
                SPARSE_WRITE_BUFFERS.lock().insert(key, buffers);
                file_desc.fpos = saved_pos;
                if !use_active_descriptor {
                    unsafe { ext4_fclose(&mut temporary_desc) };
                }
                if r != EOK as i32 {
                    error!("sparse write buffer ext4_fwrite: rc = {}", r);
                    return Err(r);
                }
                error!(
                    "sparse write buffer short write: expected {}, got {}",
                    expected, rw_count
                );
                return Err(EIO as i32);
            }
            largest_end = largest_end.max(end as u64);
            #[cfg(feature = "perf")]
            perf::record_sparse_write_flush(reason, rw_count);
        }
        file_desc.fpos = saved_pos;

        let close_r = if use_active_descriptor {
            EOK as i32
        } else {
            unsafe { ext4_fclose(&mut temporary_desc) }
        };
        if close_r != EOK as i32 {
            error!("sparse write buffer ext4_fclose: rc = {}", close_r);
            return Err(close_r);
        }

        self.file_desc.fsize = self.file_desc.fsize.max(largest_end);
        Ok(0)
    }

    /// Buffer one direct-write range for a sparse inode.
    ///
    /// Unlike the whole-file cache this never fills holes with zero bytes:
    /// every eventual `ext4_fwrite()` starts at the original offset and only
    /// covers bytes supplied by connected writes. Several disjoint ranges can
    /// coexist for one inode; only a full range set or allocation pressure
    /// commits them before the normal direct path resumes.
    fn buffer_sparse_write_at(&mut self, offset: usize, buf: &[u8]) -> Result<bool, i32> {
        if !self.sparse_write_buffer_enabled() || buf.is_empty() {
            return Ok(false);
        }
        // A following large direct write may overlap the pending range.  It
        // must not be allowed to reach lwext4 first and then be overwritten
        // by a later sparse-buffer flush.
        if buf.len() > MAX_SPARSE_WRITE_BUFFER_SIZE {
            #[cfg(feature = "perf")]
            perf::record_sparse_large_direct(buf.len());
            self.flush_sparse_write_buffer_with_cache_evict_cause(
                SparseWriteCacheEvictCause::LargeDirect,
            )?;
            return Ok(false);
        }
        let end = offset.checked_add(buf.len()).ok_or(EINVAL as i32)?;
        let Some(key) = self.whole_file_cache_key() else {
            return Ok(false);
        };

        match try_insert_sparse_write_buffer(key, offset, buf)? {
            SparseWriteBufferInsertResult::Buffered => {
                self.finish_sparse_buffered_write(end, buf.len());
                Ok(true)
            }
            SparseWriteBufferInsertResult::Flush(cause) => {
                self.flush_sparse_write_buffer_with_cache_evict_cause(cause)?;
                if matches!(
                    try_insert_sparse_write_buffer(key, offset, buf)?,
                    SparseWriteBufferInsertResult::Buffered
                ) {
                    self.finish_sparse_buffered_write(end, buf.len());
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            SparseWriteBufferInsertResult::Direct => Ok(false),
        }
    }

    /// Try to buffer one sparse write without touching lwext4.
    ///
    /// This helper is safe for callers that do not hold Ya2yOS's mount-wide
    /// lwext4 gate because it never flushes existing sparse runs and never
    /// falls through to ext4_fwrite. Returning Ok(false) means the caller
    /// must use the normal serialized write path, which may publish a full
    /// sparse-buffer batch before retrying the write.
    pub fn try_buffer_sparse_write_at(&mut self, offset: usize, buf: &[u8]) -> Result<bool, i32> {
        if !self.sparse_write_buffer_enabled()
            || buf.is_empty()
            || buf.len() > MAX_SPARSE_WRITE_BUFFER_SIZE
        {
            return Ok(false);
        }
        let end = offset.checked_add(buf.len()).ok_or(EINVAL as i32)?;
        let Some(key) = self.whole_file_cache_key() else {
            return Ok(false);
        };

        if matches!(
            try_insert_sparse_write_buffer(key, offset, buf)?,
            SparseWriteBufferInsertResult::Buffered
        ) {
            self.finish_sparse_buffered_write(end, buf.len());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn finish_sparse_buffered_write(&mut self, end: usize, bytes: usize) {
        self.file_desc.fpos = end as u64;
        self.file_desc.fsize = self.file_desc.fsize.max(end as u64);
        #[cfg(feature = "perf")]
        {
            self.last_write_path = FileWritePath::SparseBuffered;
            perf::record_sparse_write_buffer(bytes);
        }
        #[cfg(not(feature = "perf"))]
        let _ = bytes;
    }

    /*
    pub fn file_close(&mut self, path: &str) -> Result<usize, i32> {
        let cstr_path = CString::new(path).unwrap();
        if let Some(mut fd) = self.file_desc_map.remove(&cstr_path) {
            unsafe {
                ext4_fclose(&mut fd);
            }
            Ok(0)
        } else {
            error!("Can't find file descriptor of {}", path);
            Err(-1)
        }
    }
    */

    pub fn file_write(&mut self, buf: &[u8]) -> Result<usize, i32> {
        let path = String::from((*self.file_path).to_str().unwrap());
        #[cfg(feature = "perf")]
        let mut direct_reason = DirectWriteReason::Uncached;
        if self.write_back_cache_enabled(&path) {
            if let Some(cache) = get_cache(&path) {
                // 找到 cache 直接写 cache；一旦文件膨胀到阈值以上，立即回退到底层 ext4。
                let mut cache_writer = cache.write();
                if !cache_writer.evicting {
                    let next_size = cache_writer
                        .offset
                        .checked_add(buf.len())
                        .ok_or(EINVAL as i32)?;
                    let write_creates_hole =
                        !buf.is_empty() && cache_writer.offset > cache_writer.size;
                    if next_size > MAX_CACHED_FILE_SIZE || write_creates_hole {
                        let write_offset = cache_writer.offset;
                        drop(cache_writer);
                        #[cfg(feature = "perf")]
                        {
                            direct_reason = if write_creates_hole {
                                DirectWriteReason::Hole
                            } else {
                                DirectWriteReason::Limit
                            };
                        }
                        if write_creates_hole {
                            self.disable_write_back_cache()?;
                        } else {
                            let _flushed = write_back_cache(path.clone())?;
                            #[cfg(feature = "perf")]
                            perf::record_write_cache_limit_flush(_flushed);
                            remove_file_cache_state(&path);
                        }
                        self.file_desc.fpos = write_offset as u64;
                    } else {
                        cache_writer.writebuf(buf)?;
                        drop(cache_writer);
                        touch_fifo_path(&path);
                        #[cfg(feature = "perf")]
                        perf::record_write_cache_hit(buf.len());
                        return Ok(buf.len());
                    }
                }
            }
        }

        let write_start = self.file_desc.fpos as usize;
        if self.buffer_sparse_write_at(write_start, buf)? {
            self.file_desc.fpos = write_start.checked_add(buf.len()).ok_or(EINVAL as i32)? as u64;
            return Ok(buf.len());
        }
        let mut rw_count = 0;
        let r = unsafe {
            ext4_fwrite(
                &mut self.file_desc,
                buf.as_ptr() as _,
                buf.len(),
                &mut rw_count,
            )
        };

        if r != EOK as i32 {
            error!("ext4_fwrite: rc = {}", r);
            return Err(r);
        }

        if write_start.saturating_add(rw_count) > MAX_CACHED_FILE_SIZE {
            self.cache_too_large = true;
        }
        #[cfg(feature = "perf")]
        {
            if direct_reason == DirectWriteReason::Uncached {
                direct_reason = if self.cache_disabled {
                    DirectWriteReason::Disabled
                } else if self.cache_too_large {
                    DirectWriteReason::TooLarge
                } else {
                    DirectWriteReason::Uncached
                };
            }
            perf::record_direct_write(direct_reason, rw_count);
        }

        //debug!("file_write {:?}, len={}", self.get_path(), rw_count);
        Ok(rw_count)
    }

    /// Write at an explicit offset without performing a separate seek.
    ///
    /// VFS `write_at` already owns the file offset, and mmap16 issues many
    /// adjacent 1 KiB writes. Combining the cache lookup, offset update and
    /// write avoids two global cache-table locks and a second cache policy
    /// probe for every small write.
    pub fn file_write_at(&mut self, offset: usize, buf: &[u8]) -> Result<usize, i32> {
        #[cfg(feature = "perf")]
        let mut direct_reason = DirectWriteReason::Uncached;
        if !self.cache_disabled && !self.cache_too_large {
            let mut cache = CACHE_TABLE.lock().get(self.path_str()).cloned();
            if cache.is_none() && !self.whole_file_cache_disabled() {
                self.check_cached(String::from(self.path_str()))?;
                cache = CACHE_TABLE.lock().get(self.path_str()).cloned();
            } else if cache.is_none() {
                self.cache_disabled = true;
            }

            if let Some(cache) = cache {
                let mut cache_writer = cache.write();
                let next_size = offset.checked_add(buf.len()).ok_or(EINVAL as i32)?;
                let write_creates_hole = !buf.is_empty() && offset > cache_writer.size;
                if !cache_writer.evicting {
                    if next_size <= MAX_CACHED_FILE_SIZE && !write_creates_hole {
                        cache_writer.offset = offset;
                        cache_writer.writebuf(buf)?;
                        drop(cache_writer);
                        touch_fifo_path(self.path_str());
                        #[cfg(feature = "perf")]
                        {
                            self.last_write_path = FileWritePath::DenseWriteBack;
                            perf::record_write_cache_hit(buf.len());
                        }
                        return Ok(buf.len());
                    }

                    drop(cache_writer);
                    #[cfg(feature = "perf")]
                    {
                        direct_reason = if write_creates_hole {
                            DirectWriteReason::Hole
                        } else {
                            DirectWriteReason::Limit
                        };
                    }
                    if write_creates_hole {
                        self.disable_write_back_cache()?;
                    } else {
                        let _flushed = write_back_cache(String::from(self.path_str()))?;
                        #[cfg(feature = "perf")]
                        perf::record_write_cache_limit_flush(_flushed);
                        remove_file_cache_state(self.path_str());
                    }
                }
            }
        }

        if self.buffer_sparse_write_at(offset, buf)? {
            self.file_desc.fpos = offset.checked_add(buf.len()).ok_or(EINVAL as i32)? as u64;
            #[cfg(feature = "perf")]
            {
                self.last_write_path = FileWritePath::SparseBuffered;
            }
            return Ok(buf.len());
        }

        self.file_desc.fpos = offset as u64;
        let mut rw_count = 0;
        let r = unsafe {
            ext4_fwrite(
                &mut self.file_desc,
                buf.as_ptr() as _,
                buf.len(),
                &mut rw_count,
            )
        };
        if r != EOK as i32 {
            error!("ext4_fwrite: rc = {}", r);
            return Err(r);
        }
        if offset.saturating_add(rw_count) > MAX_CACHED_FILE_SIZE {
            self.cache_too_large = true;
        }
        #[cfg(feature = "perf")]
        {
            self.last_write_path = FileWritePath::Direct;
            if direct_reason == DirectWriteReason::Uncached {
                direct_reason = if self.cache_disabled {
                    DirectWriteReason::Disabled
                } else if self.cache_too_large {
                    DirectWriteReason::TooLarge
                } else {
                    DirectWriteReason::Uncached
                };
            }
            perf::record_direct_write(direct_reason, rw_count);
        }
        Ok(rw_count)
    }

    #[cfg(feature = "perf")]
    #[inline]
    pub fn last_write_path(&self) -> FileWritePath {
        self.last_write_path
    }

    pub fn file_truncate(&mut self, size: u64) -> Result<usize, i32> {
        debug!("file_truncate to {}", size);

        // Truncation changes EOF and may discard/extend extents.  Commit the
        // pending range first so the operation observes the same ordering as
        // a sequence of direct pwrite calls.
        self.flush_sparse_write_buffer(SparseWriteFlushReason::Truncate)?;
        self.disable_write_back_cache()?;

        let r = unsafe { ext4_ftruncate(&mut self.file_desc, size) };
        if r != EOK as i32 {
            error!("ext4_ftruncate: rc = {}", r);
            return Err(r);
        }
        if size == 0 {
            if let Some(key) = self.whole_file_cache_key() {
                clear_whole_file_cache_policy(key);
            }
            self.cache_disabled = false;
        }
        self.cache_too_large = size > MAX_CACHED_FILE_SIZE as u64;
        Ok(EOK as usize)
    }

    pub fn file_size(&mut self) -> u64 {
        let path = String::from((*self.file_path).to_str().unwrap());
        if self.write_back_cache_enabled(&path) {
            if let Some(cache) = get_cache(&path) {
                let cache = cache.read();
                if !cache.evicting {
                    return cache.size as u64;
                }
            }
        }
        let sparse_end = self.sparse_write_buffer_end();

        // Query the size through a separate descriptor.  Reusing
        // `self.file_desc` here would replace an active O_RDWR descriptor
        // with O_RDONLY while leaving `last_flags` unchanged; the next write
        // could then be rejected by lwext4 as a read-only operation.
        let c_path = self.file_path.clone().into_raw();
        let c_flags = Ext4File::flags_to_cstring(O_RDONLY).into_raw();
        let mut size_desc = ext4_file {
            mp: core::ptr::null_mut(),
            inode: 0,
            flags: 0,
            fsize: 0,
            fpos: 0,
        };

        // 重新打开文件获得最新的文件信息，但不要覆盖当前活动句柄。
        let r = unsafe { ext4_fopen(&mut size_desc, c_path, c_flags) };
        unsafe {
            // deallocate the CString
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_flags));
        }
        if r != EOK as i32 {
            return sparse_end.unwrap_or(0) as u64;
        }
        let size = unsafe { ext4_fsize(&mut size_desc) };
        unsafe {
            ext4_fclose(&mut size_desc);
        }
        // Keep SEEK_END and direct writes based on the current EOF without
        // replacing the active descriptor's flags or file position.
        let size = size.max(sparse_end.unwrap_or(0) as u64);
        self.file_desc.fsize = size;
        size
    }

    pub fn file_cache_flush(&mut self) -> Result<usize, i32> {
        self.file_cache_flush_with_sparse_reason(SparseWriteFlushReason::Other)
    }

    fn file_cache_flush_with_sparse_reason(
        &mut self,
        reason: SparseWriteFlushReason,
    ) -> Result<usize, i32> {
        self.flush_sparse_write_buffer(reason)?;
        let path = String::from((*self.file_path).to_str().unwrap());
        if self.write_back_cache_enabled(&path) && if_cache(path.clone()) {
            write_back_cache(path.clone())?;
        }

        self.flush_ext4_block_cache()
    }

    fn flush_ext4_block_cache(&mut self) -> Result<usize, i32> {
        let path = self.file_path.to_str().expect("invalid ext4 file path");
        flush_ext4_block_cache_for_path(path)
    }

    /// Write the pathname's delayed byte cache into lwext4, retaining the
    /// cache entry on failure so the caller can retry without losing data.
    fn write_back_path_cache(&mut self, reason: SparseWriteFlushReason) -> Result<String, i32> {
        self.flush_sparse_write_buffer(reason)?;
        let path = String::from((*self.file_path).to_str().unwrap());
        // This path can be transitioning to the non-cacheable state. Flush a
        // pre-existing entry even after its policy was marked disabled.
        if if_cache(path.clone()) {
            write_back_cache(path.clone())?;
        }
        Ok(path)
    }

    /// Write back and discard delayed state before renaming a pathname.
    ///
    /// Dense byte caches must be persisted before the source pathname is
    /// detached; otherwise a later close can recreate or overwrite the
    /// temporary path. Durability remains the responsibility of the existing
    /// sync/fsync paths.
    pub fn write_back_and_discard_path_cache(&mut self) -> Result<usize, i32> {
        #[cfg(feature = "perf")]
        return self.write_back_and_discard_path_cache_with_perf_observer(|_| {});
        #[cfg(not(feature = "perf"))]
        self.write_back_and_discard_path_cache_with_stage_observer(())
    }

    /// Rename visibility barrier with synchronous, perf-only stage events.
    /// The observer reports only boundaries and cannot alter the cache or I/O
    /// decisions made by the normal rename path.
    #[cfg(feature = "perf")]
    pub fn write_back_and_discard_path_cache_with_perf_observer(
        &mut self,
        observer: impl FnMut(RenameWriteBackStageEvent),
    ) -> Result<usize, i32> {
        self.write_back_and_discard_path_cache_with_stage_observer(observer)
    }

    fn write_back_and_discard_path_cache_with_stage_observer<O: RenameWriteBackStageObserver>(
        &mut self,
        mut observer: O,
    ) -> Result<usize, i32> {
        #[cfg(not(feature = "perf"))]
        let _ = &mut observer;

        #[cfg(feature = "perf")]
        observer.stage(RenameWriteBackStageEvent::SparseWriteFlushBegin);
        let sparse_flush = self.flush_sparse_write_buffer(SparseWriteFlushReason::Rename);
        #[cfg(feature = "perf")]
        observer.stage(RenameWriteBackStageEvent::SparseWriteFlushEnd);
        let sparse_bytes = sparse_flush?;

        let path = String::from((*self.file_path).to_str().unwrap());
        #[cfg(feature = "perf")]
        observer.stage(RenameWriteBackStageEvent::DenseWriteBackBegin);
        let dense_write_back = if if_cache(path.clone()) {
            write_back_cache(path.clone())
        } else {
            Ok(0)
        };
        #[cfg(feature = "perf")]
        observer.stage(RenameWriteBackStageEvent::DenseWriteBackEnd);
        let dense_bytes = dense_write_back?;

        #[cfg(not(feature = "perf"))]
        let _ = (sparse_bytes, dense_bytes);

        #[cfg(feature = "perf")]
        observer.stage(RenameWriteBackStageEvent::PathCacheDiscardBegin);
        discard_path_cache(&path);
        #[cfg(feature = "perf")]
        {
            observer.stage(RenameWriteBackStageEvent::PathCacheDiscardEnd);
            perf::record_rename_write_back(sparse_bytes, dense_bytes);
            perf::record_rename_path_cache_discard();
        }
        Ok(0)
    }

    /// Persist and discard delayed write-back state before an operation that
    /// requires a mount-wide block-cache flush.  Keeping a dirty cache under
    /// the old pathname after that transition could otherwise recreate the
    /// pathname on a later close or eviction.
    pub fn flush_and_discard_path_cache(&mut self) -> Result<usize, i32> {
        let path = self.write_back_path_cache(SparseWriteFlushReason::Other)?;
        self.flush_ext4_block_cache()?;
        discard_path_cache(&path);
        Ok(0)
    }

    fn whole_file_cache_key(&self) -> Option<WholeFileCacheKey> {
        whole_file_cache_key_for_desc(&self.file_desc)
    }

    fn whole_file_cache_disabled(&self) -> bool {
        whole_file_cache_disabled_for_desc(&self.file_desc)
    }

    fn disable_whole_file_cache(&self) {
        if let Some(key) = self.whole_file_cache_key() {
            WHOLE_FILE_CACHE_DISABLED_INODES.lock().insert(key);
        }
    }

    fn write_back_cache_enabled(&mut self, _path: &str) -> bool {
        if self.cache_disabled || self.whole_file_cache_disabled() {
            self.cache_disabled = true;
            return false;
        }
        true
    }

    /// A whole-file cache stores bytes but not extents. Disable it before an
    /// operation that can create or inspect holes. The policy is path-global:
    /// separate open file descriptions must not recreate the cache later.
    pub fn disable_write_back_cache(&mut self) -> Result<usize, i32> {
        let path = String::from((*self.file_path).to_str().unwrap());
        if self.cache_disabled || self.whole_file_cache_disabled() {
            self.cache_disabled = true;
            if let Some(key) = self.whole_file_cache_key() {
                discard_inode_caches(key);
            }
            return Ok(0);
        }

        if let Some(key) = self.whole_file_cache_key() {
            // Persist every alias's existing cache before this inode becomes
            // sparse.  Keep the entries until the underlying ext4 cache has
            // flushed as well, so a failed flush leaves caller data available
            // for a retry instead of silently discarding it.
            flush_inode_caches(key)?;
            self.flush_ext4_block_cache()?;
            discard_inode_caches(key);
        } else if if_cache(path.clone()) {
            self.flush_and_discard_path_cache()?;
        }

        self.disable_whole_file_cache();
        self.cache_disabled = true;
        Ok(0)
    }

    /// Keep this descriptor's cache out of the global FIFO while the inode is
    /// still actively used after unlink.  The owner removes it through the
    /// normal path/inode cache cleanup when the last reference is dropped.
    pub fn pin_write_back_cache(&mut self) {
        self.cache_pinned = true;
        let path = self.file_path.to_str().unwrap();
        remove_fifo_path(path);
    }

    pub fn set_time(
        &mut self,
        atime: Option<u64>,
        mtime: Option<u64>,
        ctime: Option<u64>,
    ) -> Result<usize, i32> {
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let mut r = 0;
        if let Some(atime) = atime {
            r = unsafe { ext4_atime_set(c_path, atime) }
        }
        if let Some(mtime) = mtime {
            r = unsafe { ext4_mtime_set(c_path, mtime) }
        }
        if let Some(ctime) = ctime {
            r = unsafe { ext4_ctime_set(c_path, ctime) }
        }
        // unsafe { ext4_mode_set(c_path, mode) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_time_set: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }
    // Ok(atime,mtime,ctime)
    // pub fn time(&mut self) -> Result<(u32, u32, u32), i32> {
    //     let (mut atime, mut mtime, mut ctime) = (0, 0, 0);
    //     let c_path = self.file_path.clone();
    //     let c_path = c_path.into_raw();
    //     let r = unsafe {
    //         ext4_atime_get(c_path, &mut atime)
    //             | ext4_mtime_get(c_path, &mut mtime)
    //             | ext4_ctime_get(c_path, &mut ctime)
    //     };
    //     unsafe {
    //         drop(CString::from_raw(c_path));
    //     }
    //     if r != EOK as i32 {
    //         error!("ext4_mode_get: rc = {}", r);
    //         return Err(r);
    //     }
    //     Ok((atime, mtime, ctime))
    // }
    /// Query inode metadata without exposing perf stage events.
    pub fn fstat(&mut self) -> Result<ext4_inode_stat, i32> {
        #[cfg(feature = "perf")]
        return self.fstat_with_stage_observer(|_| {});
        #[cfg(not(feature = "perf"))]
        self.fstat_with_stage_observer(())
    }

    /// Query inode metadata while reporting the exact wrapper boundaries
    /// needed by Ya2yOS's aggregate fstat profiler. The observer is invoked
    /// synchronously and does not alter fstat's cache, I/O, or error paths.
    #[cfg(feature = "perf")]
    pub fn fstat_with_perf_observer(
        &mut self,
        observer: impl FnMut(FstatStageEvent),
    ) -> Result<ext4_inode_stat, i32> {
        self.fstat_with_stage_observer(observer)
    }

    fn fstat_with_stage_observer<O: FstatStageObserver>(
        &mut self,
        mut observer: O,
    ) -> Result<ext4_inode_stat, i32> {
        #[cfg(not(feature = "perf"))]
        let _ = &mut observer;
        #[cfg(feature = "perf")]
        perf::record_fstat_call();
        // `st_blocks` as well as `st_size` are observable through fstat, so
        // publish a pending sparse range instead of fabricating allocation
        // metadata from an in-memory byte buffer.
        #[cfg(feature = "perf")]
        observer.stage(FstatStageEvent::SparseWriteFlushBegin);
        let sparse_flush = self.flush_sparse_write_buffer(SparseWriteFlushReason::Fstat);
        #[cfg(feature = "perf")]
        observer.stage(FstatStageEvent::SparseWriteFlushEnd);
        sparse_flush?;
        let path = String::from((*self.file_path).to_str().unwrap());
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let mut stat = ext4_inode_stat::default();
        #[cfg(feature = "perf")]
        {
            perf::record_fstat_stat_get();
            observer.stage(FstatStageEvent::Ext4StatGetBegin);
        }
        let r = unsafe { ext4_stat_get(c_path, &mut stat) };
        #[cfg(feature = "perf")]
        observer.stage(FstatStageEvent::Ext4StatGetEnd);

        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            // Small files can be visible only through the write-back cache
            // until their first flush. Keep fstat usable for that transient
            // state instead of reporting a spurious filesystem error.
            if let Some(cache) = get_cache(&path) {
                #[cfg(feature = "perf")]
                {
                    perf::record_fstat_write_back_fallback();
                    observer.stage(FstatStageEvent::WriteBackFallbackBegin);
                }
                let cache = cache.read();
                if !cache.evicting {
                    stat.st_mode = cache.mode.unwrap_or(0o100000);
                    stat.st_nlink = 1;
                    stat.st_size = cache.size as isize;
                    stat.st_blksize = 512;
                    stat.st_blocks = ((cache.size + 511) / 512) as isize;
                    #[cfg(feature = "perf")]
                    observer.stage(FstatStageEvent::WriteBackFallbackEnd);
                    return Ok(stat);
                }
            }
            error!("ext4_stat_get: rc = {}", r);
            return Err(r);
        }

        if let Some(cache) = get_cache(&path) {
            //如果在缓存中，更新stat获得的大小
            #[cfg(feature = "perf")]
            {
                perf::record_fstat_write_back_overlay();
                observer.stage(FstatStageEvent::WriteBackOverlayBegin);
            }
            let cache_reader = cache.read();
            if !cache_reader.evicting {
                stat.st_size = cache_reader.size as isize;
                stat.st_blocks =
                    (stat.st_size - 1 + (stat.st_blksize as isize)) / (stat.st_blksize as isize);
            }
            #[cfg(feature = "perf")]
            observer.stage(FstatStageEvent::WriteBackOverlayEnd);
        }

        // error!(
        //     "[fstat:lwext4/src/file.rs] st_atime = {},st_ctime = {},st_mtime = {}",
        //     stat.st_atime, stat.st_ctime, stat.st_mtime
        // );
        Ok(stat)
    }

    pub fn links_cnt(&mut self) -> Result<u32, i32> {
        let mut cnt: u32 = 0;
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_get_links_cnt(c_path, &mut cnt) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            // error!("ext4_links_cnt_get: rc = {}", r);
            return Err(r);
        }
        Ok(cnt)
    }
    pub fn file_mode(&mut self) -> Result<u32, i32> {
        // 0o777 (octal) == rwxrwxrwx
        let mut mode: u32 = 0o777;
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_mode_get(c_path, &mut mode) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_mode_get: rc = {}", r);
            return Err(r);
        }
        //debug!("Got file mode={:#x}", mode);
        Ok(mode)
    }

    pub fn file_mode_set(&mut self, mode: u32) -> Result<usize, i32> {
        //debug!("file_mode_set to {:#x}", mode);

        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_mode_set(c_path, mode) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_mode_set: rc = {}", r);
            return Err(r);
        }
        self.pending_mode = Some(mode);
        let path = String::from((*self.file_path).to_str().unwrap());
        if let Some(cache) = get_cache(&path) {
            let mut cache = cache.write();
            if !cache.evicting {
                cache.mode = Some(mode);
            }
        }
        Ok(EOK as usize)
    }

    pub fn file_mode_owner_set(&mut self, mode: u32, uid: u32, gid: u32) -> Result<usize, i32> {
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_mode_owner_set_raw(c_path, mode, uid, gid) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_mode_owner_set: rc = {}", r);
            return Err(r);
        }
        self.pending_mode = Some(mode);
        let path = String::from((*self.file_path).to_str().unwrap());
        if let Some(cache) = get_cache(&path) {
            let mut cache = cache.write();
            if !cache.evicting {
                cache.mode = Some(mode);
            }
        }
        Ok(EOK as usize)
    }

    pub fn file_owner_set(&mut self, uid: u32, gid: u32) -> Result<usize, i32> {
        // chown/fchownat need the on-disk inode owner, not just cached stat data.
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_owner_set(c_path, uid, gid) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_owner_set: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    pub fn file_type(&mut self) -> InodeTypes {
        let mode = self.file_mode();
        if mode.is_err() {
            warn!("file_type mode.is_err");
            return InodeTypes::EXT4_INODE_MODE_FILE;
        }
        let mode = mode.unwrap();
        let types = mode & 0o170000;
        let itypes = match types {
            0x1000 => InodeTypes::EXT4_INODE_MODE_FIFO,
            0x2000 => InodeTypes::EXT4_INODE_MODE_CHARDEV,
            0x4000 => InodeTypes::EXT4_INODE_MODE_DIRECTORY,
            0x6000 => InodeTypes::EXT4_INODE_MODE_BLOCKDEV,
            0x8000 => InodeTypes::EXT4_INODE_MODE_FILE,
            0xA000 => InodeTypes::EXT4_INODE_MODE_SOFTLINK,
            0xC000 => InodeTypes::EXT4_INODE_MODE_SOCKET,
            0xF000 => InodeTypes::EXT4_INODE_MODE_TYPE_MASK,
            _ => {
                warn!("Unknown inode mode type {:x}", types);
                InodeTypes::EXT4_INODE_MODE_FILE
            }
        };
        //debug!("Inode mode types: {:?}", itypes);

        itypes
    }

    /********* DIRECTORY OPERATION *********/

    /// Create new directory
    pub fn dir_mk(&mut self, path: &str) -> Result<usize, i32> {
        //debug!("directory create: {}", path);
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();

        let r = unsafe { ext4_dir_mk(c_path) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_dir_mk: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Create a directory while preserving O_CREAT|O_EXCL semantics in the
    /// same lwext4 pathname traversal.
    pub fn dir_mk_exclusive(&mut self, path: &str) -> Result<usize, i32> {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_dir_mk_exclusive_raw(c_path) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_dir_mk_exclusive: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Create a directory with its final mode and owner in the creation
    /// transaction, preserving O_EXCL semantics.
    pub fn dir_mk_exclusive_with_metadata(
        &mut self,
        path: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<ext4_inode_stat, i32> {
        let c_path = CString::new(path).expect("CString::new failed").into_raw();
        let mut stat = ext4_inode_stat::default();
        let r =
            unsafe { ext4_dir_mk_exclusive_with_metadata_raw(c_path, mode, uid, gid, &mut stat) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            if r == EEXIST as i32 {
                debug!("ext4_dir_mk_exclusive_with_metadata: {}, rc = {}", path, r);
            } else {
                error!("ext4_dir_mk_exclusive_with_metadata: {}, rc = {}", path, r);
            }
            return Err(r);
        }
        Ok(stat)
    }

    /// Rename/move directory
    pub fn dir_mv(&mut self, path: &str, new_path: &str) -> Result<usize, i32> {
        //debug!("directory move from {} to {}", path, new_path);

        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let c_new_path = CString::new(new_path).expect("CString::new failed");
        let c_new_path = c_new_path.into_raw();

        let r = unsafe { ext4_dir_mv(c_path, c_new_path) };
        unsafe {
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_new_path));
        }
        if r != EOK as i32 {
            error!("ext4_dir_mv: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Recursive directory remove
    pub fn dir_rm(&mut self, path: &str) -> Result<usize, i32> {
        //debug!("directory recursive remove: {}", path);

        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();

        let r = unsafe { ext4_dir_rm(c_path) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if (r != EOK as i32) && (r != ENOENT as i32) {
            error!("ext4_fremove ext4_dir_rm: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    pub fn read_dir_from(&self, off: u64) -> Result<Vec<OsDirent>, i32> {
        if self.this_type != InodeTypes::EXT4_DE_DIR {
            return Err(22);
        }
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let mut d: ext4_dir = unsafe { core::mem::zeroed() };
        let mut entries: Vec<_> = Vec::new();

        unsafe {
            ext4_dir_open(&mut d, c_path);
            drop(CString::from_raw(c_path));
            d.next_off = off;
            let mut de = ext4_dir_entry_next(&mut d);
            while !de.is_null() {
                let dentry = &(*de);
                //对齐 align8
                let mut name = [0u8; 256];
                let name_len = dentry.name_length as usize;
                name[0..name_len].copy_from_slice(&dentry.name[0..name_len]);
                let mut len = name_len + 19;
                let align = 8 - len % 8;
                len += align;
                entries.push(OsDirent {
                    d_ino: dentry.inode as u64,
                    d_off: d.next_off as i64,
                    d_reclen: len as u16,
                    d_type: ext4_dirent_type_to_linux_dtype(dentry.inode_type),
                    d_name: name,
                });
                de = ext4_dir_entry_next(&mut d);
            }
            ext4_dir_close(&mut d);
        }
        Ok(entries)
    }

    /// SEEK_DATA: find next data offset >= `offset`.
    /// Returns ENXIO if there is no data at or after `offset`.
    pub fn file_seek_data(&mut self, offset: u64) -> Result<u64, i32> {
        // SEEK_DATA/SEEK_HOLE expose allocation layout, not merely bytes.
        // Commit the exact dirty range before asking lwext4 to inspect holes.
        self.flush_sparse_write_buffer(SparseWriteFlushReason::Other)?;
        let mut result: u64 = 0;
        let rc = unsafe { ext4_fseek_data_raw(&mut self.file_desc, offset, &mut result) };
        if rc != EOK as i32 {
            error!("ext4_fseek_data: rc = {}", rc);
            return Err(rc);
        }
        Ok(result)
    }

    /// SEEK_HOLE: find next hole offset >= `offset`.
    /// Returns ENXIO for offsets at or beyond EOF.
    pub fn file_seek_hole(&mut self, offset: u64) -> Result<u64, i32> {
        self.flush_sparse_write_buffer(SparseWriteFlushReason::Other)?;
        let mut result: u64 = 0;
        let rc = unsafe { ext4_fseek_hole_raw(&mut self.file_desc, offset, &mut result) };
        if rc != EOK as i32 {
            error!("ext4_fseek_hole: rc = {}", rc);
            return Err(rc);
        }
        Ok(result)
    }
}

/*
pub enum OpenFlags {
O_RDONLY = 0,
O_WRONLY = 0x1,
O_RDWR = 0x2,
O_CREAT = 0x40,
O_TRUNC = 0x200,
O_APPEND = 0x400,
}
*/

#[derive(PartialEq, Clone, Debug)]
pub enum InodeTypes {
    // Inode type, Directory entry types.
    EXT4_DE_UNKNOWN = 0,
    EXT4_DE_REG_FILE = 1,
    EXT4_DE_DIR = 2,
    EXT4_DE_CHRDEV = 3,
    EXT4_DE_BLKDEV = 4,
    EXT4_DE_FIFO = 5,
    EXT4_DE_SOCK = 6,
    EXT4_DE_SYMLINK = 7,

    // Inode mode
    EXT4_INODE_MODE_FIFO = 0x1000,
    EXT4_INODE_MODE_CHARDEV = 0x2000,
    EXT4_INODE_MODE_DIRECTORY = 0x4000,
    EXT4_INODE_MODE_BLOCKDEV = 0x6000,
    EXT4_INODE_MODE_FILE = 0x8000,
    EXT4_INODE_MODE_SOFTLINK = 0xA000,
    EXT4_INODE_MODE_SOCKET = 0xC000,
    EXT4_INODE_MODE_TYPE_MASK = 0xF000,
}

impl From<usize> for InodeTypes {
    fn from(num: usize) -> InodeTypes {
        match num {
            0 => InodeTypes::EXT4_DE_UNKNOWN,
            1 => InodeTypes::EXT4_DE_REG_FILE,
            2 => InodeTypes::EXT4_DE_DIR,
            3 => InodeTypes::EXT4_DE_CHRDEV,
            4 => InodeTypes::EXT4_DE_BLKDEV,
            5 => InodeTypes::EXT4_DE_FIFO,
            6 => InodeTypes::EXT4_DE_SOCK,
            7 => InodeTypes::EXT4_DE_SYMLINK,
            0x1000 => InodeTypes::EXT4_INODE_MODE_FIFO,
            0x2000 => InodeTypes::EXT4_INODE_MODE_CHARDEV,
            0x4000 => InodeTypes::EXT4_INODE_MODE_DIRECTORY,
            0x6000 => InodeTypes::EXT4_INODE_MODE_BLOCKDEV,
            0x8000 => InodeTypes::EXT4_INODE_MODE_FILE,
            0xA000 => InodeTypes::EXT4_INODE_MODE_SOFTLINK,
            0xC000 => InodeTypes::EXT4_INODE_MODE_SOCKET,
            0xF000 => InodeTypes::EXT4_INODE_MODE_TYPE_MASK,
            _ => {
                warn!("Unknown ext4 inode type: {}", num);
                InodeTypes::EXT4_DE_UNKNOWN
            }
        }
    }
}
#[repr(C)]
#[derive(Debug)]
pub struct OsDirent {
    pub d_ino: u64,        // 索引节点号
    pub d_off: i64,        // 从 0 开始到下一个 dirent 的偏移
    pub d_reclen: u16,     // 当前 dirent 的长度
    pub d_type: u8,        // 文件类型
    pub d_name: [u8; 256], // 文件名
}

impl OsDirent {
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.d_reclen as usize
    }
    #[inline(always)]
    pub fn off(&self) -> usize {
        self.d_off as usize
    }

    /// 将当前目录项解释为字节切片，用于 getdents64 序列化
    pub fn as_bytes(&self) -> &[u8] {
        // 名字数组大小不定，按 d_reclen 解释
        unsafe { core::slice::from_raw_parts(self as *const _ as *const u8, self.len()) }
    }
}

#[derive(Clone)]
pub struct VFileCache {
    data: Vec<u8>,
    offset: usize,
    modified: bool,
    /// Monotonically identifies the byte image currently held in `data`.
    /// A write-back only clears `modified` when this still matches its
    /// snapshot, so a concurrent writer never loses its dirty state.
    revision: u64,
    /// FIFO eviction sets this after the data is clean. Writers which already
    /// hold an Arc then take the direct path instead of repopulating an entry
    /// that is about to be removed from the global table.
    evicting: bool,
    size: usize,
    mode: Option<u32>,
    inode_key: Option<WholeFileCacheKey>,
}

impl VFileCache {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            offset: 0,
            modified: false,
            revision: 0,
            evicting: false,
            size: 0,
            mode: None,
            inode_key: None,
        }
    }

    pub fn get_data_slice(&self) -> &[u8] {
        &self.data.as_slice()[..]
    }

    pub fn writebuf(&mut self, buf: &[u8]) -> Result<usize, i32> {
        let length = buf.len();
        let end = self.offset.checked_add(length).ok_or(EINVAL as i32)?;
        if end > self.size {
            self.size = end;
        }
        if end > self.data.len() {
            let aligned_size = aligned_down(end)
                .checked_add(PAGE_SIZE)
                .ok_or(ENOMEM as i32)?;
            let additional = aligned_size.saturating_sub(self.data.capacity());
            if additional > 0 {
                self.data
                    .try_reserve_exact(additional)
                    .map_err(|_| ENOMEM as i32)?;
            }
            self.data.resize(aligned_size, 0);
        }
        if length <= 10 {
            for i in 0..length {
                self.data[self.offset + i] = buf[i];
            }
        } else {
            self.data[self.offset..end].copy_from_slice(buf);
        }
        self.modified = true;
        self.revision = self.revision.wrapping_add(1);
        /*
        debug!(
            "write {} bytes and size is {}, data.len() is {} now",
            length,
            self.size,
            self.data.len()
        );
        */
        Ok(self.data.len())
    }

    pub fn truncate(&mut self, new_size: usize) -> Result<(), i32> {
        let aligned_size = aligned_down(new_size) + PAGE_SIZE;
        let additional = aligned_size.saturating_sub(self.data.capacity());
        if additional > 0 {
            self.data
                .try_reserve_exact(additional)
                .map_err(|_| ENOMEM as i32)?;
        }
        self.data.resize(aligned_size, 0);
        let length = self.data.len();
        if new_size < length {
            self.data[new_size..length].fill(0);
        }
        self.size = new_size;
        Ok(())
    }
}

/// A lock for one `VFileCache` entry.
///
/// `spin::RwLock` is retained only as the standalone fallback.  In the
/// kernel, the paired callbacks below use a scheduler-aware per-resource
/// rwlock.  This prevents a task preempted while touching a cache entry from
/// being starved by another hart spinning in `spin::RwLock::write()`.
struct VFileCacheLock {
    fallback: RwLock<()>,
    /// Serializes snapshot write-back operations only. Normal readers and
    /// writers use `data` and therefore remain parallel with device I/O.
    flush: VFileCacheFlushLock,
    data: UnsafeCell<VFileCache>,
}

unsafe impl Send for VFileCacheLock {}
unsafe impl Sync for VFileCacheLock {}

struct VFileCacheHookGuard {
    ctx: usize,
    lock: *mut c_void,
    unlock: VFileCacheLockHook,
    write: bool,
}

impl Drop for VFileCacheHookGuard {
    fn drop(&mut self) {
        unsafe { (self.unlock)(self.ctx as *mut c_void, self.lock, self.write) };
    }
}

/// An exclusive, task-aware gate for one cache entry's write-back stream.
/// Keeping this separate from the data lock prevents a slow lwext4 operation
/// from blocking readers or dirtying writers while still preserving the order
/// of independent snapshots sent to disk.
struct VFileCacheFlushLock {
    fallback: Mutex<()>,
}

struct VFileCacheFlushGuard<'a> {
    _backend: VFileCacheFlushBackend<'a>,
}

#[allow(dead_code)]
enum VFileCacheFlushBackend<'a> {
    Hook(VFileCacheHookGuard),
    Fallback(MutexGuard<'a, ()>),
}

#[allow(dead_code)]
enum VFileCacheReadBackend<'a> {
    Hook(VFileCacheHookGuard),
    Fallback(RwLockReadGuard<'a, ()>),
}

#[allow(dead_code)]
enum VFileCacheWriteBackend<'a> {
    Hook(VFileCacheHookGuard),
    Fallback(RwLockWriteGuard<'a, ()>),
}

struct VFileCacheReadGuard<'a> {
    lock: &'a VFileCacheLock,
    _backend: VFileCacheReadBackend<'a>,
}

struct VFileCacheWriteGuard<'a> {
    lock: &'a VFileCacheLock,
    _backend: VFileCacheWriteBackend<'a>,
}

impl VFileCacheLock {
    fn new(cache: VFileCache) -> Self {
        Self {
            fallback: RwLock::new(()),
            flush: VFileCacheFlushLock {
                fallback: Mutex::new(()),
            },
            data: UnsafeCell::new(cache),
        }
    }

    fn hook_guard(lock_ptr: *mut c_void, write: bool) -> Option<VFileCacheHookGuard> {
        let hooks = *VFILE_CACHE_LOCK_HOOKS.lock();
        let (Some(lock), Some(unlock)) = (hooks.lock, hooks.unlock) else {
            return None;
        };
        unsafe { lock(hooks.ctx as *mut c_void, lock_ptr, write) };
        Some(VFileCacheHookGuard {
            ctx: hooks.ctx,
            lock: lock_ptr,
            unlock,
            write,
        })
    }

    fn read(&self) -> VFileCacheReadGuard<'_> {
        let lock_ptr = self as *const Self as *mut c_void;
        let backend = match Self::hook_guard(lock_ptr, false) {
            Some(guard) => VFileCacheReadBackend::Hook(guard),
            None => VFileCacheReadBackend::Fallback(self.fallback.read()),
        };
        VFileCacheReadGuard {
            lock: self,
            _backend: backend,
        }
    }

    fn write(&self) -> VFileCacheWriteGuard<'_> {
        let lock_ptr = self as *const Self as *mut c_void;
        let backend = match Self::hook_guard(lock_ptr, true) {
            Some(guard) => VFileCacheWriteBackend::Hook(guard),
            None => VFileCacheWriteBackend::Fallback(self.fallback.write()),
        };
        VFileCacheWriteGuard {
            lock: self,
            _backend: backend,
        }
    }

    fn snapshot_for_writeback(&self) -> Result<Option<VFileCacheSnapshot>, i32> {
        let cache = self.read();
        if !cache.modified {
            return Ok(None);
        }

        let data = cache.data.get(..cache.size).ok_or(EIO as i32)?;
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(data.len())
            .map_err(|_| ENOMEM as i32)?;
        snapshot.extend_from_slice(data);
        Ok(Some(VFileCacheSnapshot {
            data: snapshot,
            size: cache.size,
            mode: cache.mode,
            revision: cache.revision,
        }))
    }

    fn mark_clean_if_unchanged(&self, revision: u64) {
        let mut cache = self.write();
        if cache.revision == revision {
            cache.modified = false;
        }
    }

    /// Atomically prevents new writes from using a clean entry before FIFO
    /// removes its table reference. Existing writers which race this marker
    /// observe `evicting` and use direct lwext4 I/O instead.
    fn claim_clean_for_eviction(&self) -> bool {
        let mut cache = self.write();
        if cache.modified || cache.evicting {
            return false;
        }
        cache.evicting = true;
        true
    }

    fn cancel_eviction(&self) {
        self.write().evicting = false;
    }
}

impl VFileCacheFlushLock {
    fn lock(&self) -> VFileCacheFlushGuard<'_> {
        let lock_ptr = self as *const Self as *mut c_void;
        let backend = match VFileCacheLock::hook_guard(lock_ptr, true) {
            Some(guard) => VFileCacheFlushBackend::Hook(guard),
            None => VFileCacheFlushBackend::Fallback(self.fallback.lock()),
        };
        VFileCacheFlushGuard { _backend: backend }
    }
}

impl Drop for VFileCacheLock {
    fn drop(&mut self) {
        let hooks = *VFILE_CACHE_LOCK_HOOKS.lock();
        let Some(release) = hooks.release else {
            return;
        };
        unsafe {
            release(hooks.ctx as *mut c_void, self as *mut Self as *mut c_void);
            release(
                hooks.ctx as *mut c_void,
                &mut self.flush as *mut VFileCacheFlushLock as *mut c_void,
            );
        }
    }
}

struct VFileCacheSnapshot {
    data: Vec<u8>,
    size: usize,
    mode: Option<u32>,
    revision: u64,
}

impl Deref for VFileCacheReadGuard<'_> {
    type Target = VFileCache;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl Deref for VFileCacheWriteGuard<'_> {
    type Target = VFileCache;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl DerefMut for VFileCacheWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}

//cache表，目前只为非目录文件使用cache
static CACHE_TABLE: Lazy<Mutex<BTreeMap<String, Arc<VFileCacheLock>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

type WholeFileCacheKey = (usize, u32);

/// One exact dirty run for an inode whose sparse layout rules out the
/// byte-only whole-file cache.
struct SparseWriteBuffer {
    offset: usize,
    data: Vec<u8>,
}

/// A small set of dirty ranges for one sparse inode. Rustc often alternates
/// among several output offsets, so retaining only one run would force a
/// write-back on almost every small write. A new write coalesces its connected
/// component, while disjoint ranges remain independent so holes are never
/// materialized. The byte total stays bounded and each range is later written
/// at its original offset.
struct SparseWriteBuffers {
    runs: VecDeque<SparseWriteBuffer>,
    bytes: usize,
}

/// A fallible insertion is planned before mutating the buffered ranges, so an
/// allocation failure never leaves a partially coalesced sparse inode behind.
struct SparseWriteInsertPlan {
    selected_indices: Vec<usize>,
    insert_index: usize,
    start: usize,
    len: usize,
    new_bytes: usize,
}

#[derive(Debug)]
enum SparseWriteInsertPlanError {
    Overflow,
    AllocationFailure,
}

impl SparseWriteBuffers {
    fn plan_insert(
        &mut self,
        offset: usize,
        len: usize,
    ) -> Result<SparseWriteInsertPlan, SparseWriteInsertPlanError> {
        let end = offset
            .checked_add(len)
            .ok_or(SparseWriteInsertPlanError::Overflow)?;
        // Linux writeback walks the mapping in file-offset order. Keep the
        // local dirty-range index ordered as well, so finding one connected
        // component is a linear scan instead of repeatedly searching every
        // existing range.
        self.runs
            .make_contiguous()
            .sort_unstable_by_key(|run| run.offset);
        let mut selected_indices = Vec::new();
        selected_indices
            .try_reserve(self.runs.len())
            .map_err(|_| SparseWriteInsertPlanError::AllocationFailure)?;
        let mut selected_bytes = 0usize;
        let mut start = offset;
        let mut range_end = end;

        // Because runs are sorted, a run after the first gap cannot become
        // connected later. A single write can still bridge multiple adjacent
        // ranges because range_end is extended as each one is selected.
        for (index, run) in self.runs.iter().enumerate() {
            let run_end = run
                .offset
                .checked_add(run.data.len())
                .ok_or(SparseWriteInsertPlanError::Overflow)?;
            if run_end < start {
                continue;
            }
            if run.offset > range_end {
                break;
            }
            selected_indices.push(index);
            selected_bytes = selected_bytes
                .checked_add(run.data.len())
                .ok_or(SparseWriteInsertPlanError::Overflow)?;
            start = start.min(run.offset);
            range_end = range_end.max(run_end);
        }

        let len = range_end
            .checked_sub(start)
            .ok_or(SparseWriteInsertPlanError::Overflow)?;
        let new_bytes = self
            .bytes
            .checked_sub(selected_bytes)
            .and_then(|bytes| bytes.checked_add(len))
            .ok_or(SparseWriteInsertPlanError::Overflow)?;
        let insert_index = if selected_indices.is_empty() {
            self.runs
                .iter()
                .position(|run| run.offset > offset)
                .unwrap_or(self.runs.len())
        } else {
            selected_indices[0]
        };

        Ok(SparseWriteInsertPlan {
            selected_indices,
            insert_index,
            start,
            len,
            new_bytes,
        })
    }

    fn apply_insert(
        &mut self,
        plan: SparseWriteInsertPlan,
        offset: usize,
        data: &[u8],
    ) -> Result<(), ()> {
        let mut merged_data = Vec::new();
        if merged_data.try_reserve_exact(plan.len).is_err()
            || (plan.selected_indices.is_empty() && self.runs.try_reserve_exact(1).is_err())
        {
            return Err(());
        }
        merged_data.resize(plan.len, 0);

        // Existing runs are replayed in their original write order. The new
        // write is copied last, preserving pwrite-style last-write-wins
        // semantics inside the compacted connected component.
        for (index, run) in self.runs.iter().enumerate() {
            if !plan.selected_indices.contains(&index) {
                continue;
            }
            let begin = run.offset - plan.start;
            merged_data[begin..begin + run.data.len()].copy_from_slice(&run.data);
        }
        let write_begin = offset - plan.start;
        merged_data[write_begin..write_begin + data.len()].copy_from_slice(data);

        let merged_run = SparseWriteBuffer {
            offset: plan.start,
            data: merged_data,
        };
        if plan.selected_indices.is_empty() {
            self.runs.insert(plan.insert_index, merged_run);
        } else {
            for index in plan.selected_indices.iter().rev() {
                let _ = self.runs.remove(*index);
            }
            self.runs.insert(plan.insert_index, merged_run);
        }
        self.bytes = plan.new_bytes;
        Ok(())
    }
}

enum SparseWriteBufferInsertResult {
    Buffered,
    Flush(SparseWriteCacheEvictCause),
    Direct,
}

/// The sparse payload budget is global rather than per inode. A caller
/// can only flush the pending ranges for its own open descriptor, so budget
/// pressure either publishes that inode's ranges or falls back to the normal
/// direct-write path; it never writes another inode behind its owner's back.
struct SparseWriteBufferStore {
    entries: BTreeMap<WholeFileCacheKey, SparseWriteBuffers>,
    total_bytes: usize,
}

impl SparseWriteBufferStore {
    #[inline]
    fn can_replace(&self, old_bytes: usize, new_bytes: usize) -> bool {
        new_bytes <= MAX_TOTAL_SPARSE_WRITE_BUFFER_BYTES
            && self.total_bytes.saturating_sub(old_bytes)
                <= MAX_TOTAL_SPARSE_WRITE_BUFFER_BYTES - new_bytes
    }

    #[inline]
    fn replace_bytes(&mut self, old_bytes: usize, new_bytes: usize) {
        debug_assert!(self.can_replace(old_bytes, new_bytes));
        self.total_bytes = self
            .total_bytes
            .saturating_sub(old_bytes)
            .saturating_add(new_bytes);
    }

    fn insert(&mut self, key: WholeFileCacheKey, buffers: SparseWriteBuffers) {
        let bytes = buffers.bytes;
        if let Some(previous) = self.entries.insert(key, buffers) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.bytes);
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes);
    }

    fn remove(&mut self, key: &WholeFileCacheKey) -> Option<SparseWriteBuffers> {
        let buffers = self.entries.remove(key)?;
        self.total_bytes = self.total_bytes.saturating_sub(buffers.bytes);
        Some(buffers)
    }
}

/// Insert a sparse write without exposing an intermediate state to readers.
/// The caller decides whether a capacity result may flush the current inode or
/// must fall back to direct I/O.
fn try_insert_sparse_write_buffer(
    key: WholeFileCacheKey,
    offset: usize,
    data: &[u8],
) -> Result<SparseWriteBufferInsertResult, i32> {
    let mut sparse_buffers = SPARSE_WRITE_BUFFERS.lock();
    let total_bytes = sparse_buffers.total_bytes;
    let (result, replacement) = if let Some(buffer_set) = sparse_buffers.entries.get_mut(&key) {
        let old_bytes = buffer_set.bytes;
        let plan = match buffer_set.plan_insert(offset, data.len()) {
            Ok(plan) => plan,
            Err(SparseWriteInsertPlanError::Overflow) => return Err(EFBIG as i32),
            Err(SparseWriteInsertPlanError::AllocationFailure) => {
                #[cfg(feature = "perf")]
                perf::record_sparse_buffer_allocation_failure(data.len());
                return Ok(SparseWriteBufferInsertResult::Flush(
                    SparseWriteCacheEvictCause::AllocationFailure,
                ));
            }
        };
        let budget_available = plan.new_bytes <= MAX_TOTAL_SPARSE_WRITE_BUFFER_BYTES
            && total_bytes.saturating_sub(old_bytes)
                <= MAX_TOTAL_SPARSE_WRITE_BUFFER_BYTES - plan.new_bytes;
        if plan.new_bytes > MAX_SPARSE_WRITE_BUFFER_SIZE {
            (
                SparseWriteBufferInsertResult::Flush(SparseWriteCacheEvictCause::PayloadLimit),
                None,
            )
        } else if !budget_available {
            (
                SparseWriteBufferInsertResult::Flush(SparseWriteCacheEvictCause::GlobalBudget),
                None,
            )
        } else {
            let new_bytes = plan.new_bytes;
            if buffer_set.apply_insert(plan, offset, data).is_err() {
                #[cfg(feature = "perf")]
                perf::record_sparse_buffer_allocation_failure(data.len());
                (
                    SparseWriteBufferInsertResult::Flush(
                        SparseWriteCacheEvictCause::AllocationFailure,
                    ),
                    None,
                )
            } else {
                (
                    SparseWriteBufferInsertResult::Buffered,
                    Some((old_bytes, new_bytes)),
                )
            }
        }
    } else {
        let mut buffer_set = SparseWriteBuffers {
            runs: VecDeque::new(),
            bytes: 0,
        };
        let plan = match buffer_set.plan_insert(offset, data.len()) {
            Ok(plan) => plan,
            Err(SparseWriteInsertPlanError::Overflow) => return Err(EFBIG as i32),
            Err(SparseWriteInsertPlanError::AllocationFailure) => {
                #[cfg(feature = "perf")]
                perf::record_sparse_buffer_allocation_failure(data.len());
                return Ok(SparseWriteBufferInsertResult::Direct);
            }
        };
        if plan.new_bytes > MAX_SPARSE_WRITE_BUFFER_SIZE
            || !sparse_buffers.can_replace(0, plan.new_bytes)
        {
            #[cfg(feature = "perf")]
            perf::record_sparse_buffer_budget_direct(data.len());
            (SparseWriteBufferInsertResult::Direct, None)
        } else if buffer_set.apply_insert(plan, offset, data).is_err() {
            #[cfg(feature = "perf")]
            perf::record_sparse_buffer_allocation_failure(data.len());
            (SparseWriteBufferInsertResult::Direct, None)
        } else {
            sparse_buffers.insert(key, buffer_set);
            (SparseWriteBufferInsertResult::Buffered, None)
        }
    };
    if let Some((old_bytes, new_bytes)) = replacement {
        sparse_buffers.replace_bytes(old_bytes, new_bytes);
    }
    #[cfg(feature = "perf")]
    if matches!(&result, SparseWriteBufferInsertResult::Buffered) {
        perf::record_sparse_buffer_resident_bytes(sparse_buffers.total_bytes);
    }
    Ok(result)
}

#[cfg(test)]
mod sparse_write_buffer_tests {
    use super::*;

    fn buffers() -> SparseWriteBuffers {
        SparseWriteBuffers {
            runs: VecDeque::new(),
            bytes: 0,
        }
    }

    fn insert(buffers: &mut SparseWriteBuffers, offset: usize, data: &[u8]) {
        let plan = buffers.plan_insert(offset, data.len()).unwrap();
        buffers.apply_insert(plan, offset, data).unwrap();
    }

    fn run(buffers: &SparseWriteBuffers, index: usize) -> (usize, &[u8]) {
        let run = &buffers.runs[index];
        (run.offset, &run.data)
    }

    #[test]
    fn sparse_runs_coalesce_backward_and_forward_adjacency() {
        let mut backward = buffers();
        insert(&mut backward, 4, b"ef");
        insert(&mut backward, 2, b"cd");
        assert_eq!(backward.runs.len(), 1);
        assert_eq!(run(&backward, 0), (2, b"cdef".as_slice()));

        let mut forward = buffers();
        insert(&mut forward, 2, b"cd");
        insert(&mut forward, 4, b"ef");
        assert_eq!(forward.runs.len(), 1);
        assert_eq!(run(&forward, 0), (2, b"cdef".as_slice()));
    }

    #[test]
    fn sparse_runs_coalesce_a_write_that_bridges_two_ranges() {
        let mut buffers = buffers();
        insert(&mut buffers, 0, b"ab");
        insert(&mut buffers, 4, b"ef");
        insert(&mut buffers, 2, b"cd");

        assert_eq!(buffers.runs.len(), 1);
        assert_eq!(run(&buffers, 0), (0, b"abcdef".as_slice()));
        assert_eq!(buffers.bytes, 6);
    }

    #[test]
    fn sparse_runs_preserve_last_write_wins_for_overlaps() {
        let mut buffers = buffers();
        insert(&mut buffers, 0, b"abcd");
        insert(&mut buffers, 2, b"XY");

        assert_eq!(buffers.runs.len(), 1);
        assert_eq!(run(&buffers, 0), (0, b"abXY".as_slice()));
        assert_eq!(buffers.bytes, 4);
    }

    #[test]
    fn sparse_runs_keep_holes_as_distinct_ranges() {
        let mut buffers = buffers();
        insert(&mut buffers, 0, b"ab");
        insert(&mut buffers, 4, b"ef");

        assert_eq!(buffers.runs.len(), 2);
        assert_eq!(run(&buffers, 0), (0, b"ab".as_slice()));
        assert_eq!(run(&buffers, 1), (4, b"ef".as_slice()));
        assert_eq!(buffers.bytes, 4);
    }

    #[test]
    fn sparse_ranges_do_not_have_a_bitmap_run_limit() {
        let mut buffers = buffers();
        for index in 0..128 {
            insert(&mut buffers, index * 3, b"x");
        }
        assert_eq!(buffers.runs.len(), 128);
        insert(&mut buffers, 1, b"xx");
        assert_eq!(buffers.runs.len(), 127);
        assert_eq!(run(&buffers, 0), (0, b"xxx".as_slice()));
    }

    #[test]
    fn sparse_run_coalescing_replaces_global_budget_bytes() {
        let key = (1, 1);
        let mut buffers = buffers();
        insert(&mut buffers, 0, b"abcd");
        let mut store = SparseWriteBufferStore {
            entries: BTreeMap::new(),
            total_bytes: 0,
        };
        store.insert(key, buffers);

        let old_bytes = store.entries.get(&key).unwrap().bytes;
        let plan = store
            .entries
            .get_mut(&key)
            .unwrap()
            .plan_insert(2, 2)
            .unwrap();
        assert_eq!(plan.new_bytes, 4);
        store
            .entries
            .get_mut(&key)
            .unwrap()
            .apply_insert(plan, 2, b"XY")
            .unwrap();
        store.replace_bytes(old_bytes, plan.new_bytes);

        assert_eq!(store.entries.get(&key).unwrap().bytes, 4);
        assert_eq!(store.total_bytes, 4);
    }
}

/// Entries are keyed by `(mount, inode)`, rather than pathname, so another
/// open file description observes the same pending bytes before it reads,
/// stats, renames, or synchronizes that inode.
static SPARSE_WRITE_BUFFERS: Lazy<Mutex<SparseWriteBufferStore>> = Lazy::new(|| {
    Mutex::new(SparseWriteBufferStore {
        entries: BTreeMap::new(),
        total_bytes: 0,
    })
});

// Whole-file caches represent bytes only. Once an inode has sparse layout,
// every path and open file description referring to it must bypass the cache
// so no later write-back materializes its holes as zero-filled data blocks.
static WHOLE_FILE_CACHE_DISABLED_INODES: Lazy<Mutex<BTreeSet<WholeFileCacheKey>>> =
    Lazy::new(|| Mutex::new(BTreeSet::new()));

fn clear_whole_file_cache_policy(key: WholeFileCacheKey) {
    WHOLE_FILE_CACHE_DISABLED_INODES.lock().remove(&key);
}

fn whole_file_cache_key_for_desc(file: &ext4_file) -> Option<WholeFileCacheKey> {
    if file.mp.is_null() {
        None
    } else {
        Some((file.mp as usize, file.inode))
    }
}

fn whole_file_cache_disabled_for_desc(file: &ext4_file) -> bool {
    match whole_file_cache_key_for_desc(file) {
        Some(key) => WHOLE_FILE_CACHE_DISABLED_INODES.lock().contains(&key),
        None => false,
    }
}

/// Fetch an inode-key for a path that is being unlinked without an active
/// `Ext4File` descriptor. This is best effort because cache-policy cleanup is
/// an optimization; failure must not change unlink's filesystem-visible errno.
fn whole_file_cache_key_for_path(path: &str) -> Option<WholeFileCacheKey> {
    let c_path = CString::new(path).expect("CString::new failed").into_raw();
    let flags = Ext4File::flags_to_cstring(O_RDONLY).into_raw();
    let mut file = ext4_file {
        mp: core::ptr::null_mut(),
        inode: 0,
        flags: 0,
        fsize: 0,
        fpos: 0,
    };
    let r = unsafe { ext4_fopen(&mut file, c_path, flags) };
    unsafe {
        drop(CString::from_raw(c_path));
        drop(CString::from_raw(flags));
    }
    if r != EOK as i32 {
        return None;
    }

    let key = whole_file_cache_key_for_desc(&file);
    let close_r = unsafe { ext4_fclose(&mut file) };
    if close_r != EOK as i32 {
        return None;
    }
    key
}

/// Link count lookup paired with `whole_file_cache_key_for_path()`. It is only
/// used to decide whether a successful unlink can retire inode-global cache
/// state, so lookup failure conservatively keeps that state.
fn links_cnt_for_path(path: &str) -> Option<u32> {
    let c_path = CString::new(path).expect("CString::new failed").into_raw();
    let mut count = 0;
    let r = unsafe { ext4_get_links_cnt(c_path, &mut count) };
    unsafe {
        drop(CString::from_raw(c_path));
    }
    (r == EOK as i32).then_some(count)
}

/// Returns true for a cacheable nonempty file whose on-disk layout has a hole,
/// or when lwext4 cannot safely report its layout. Large files already bypass
/// the byte-only cache, so do not linearly scan their allocation map here.
fn ext4_file_has_hole(file: &mut ext4_file) -> bool {
    let size = unsafe { ext4_fsize(file) };
    if size == 0 || size > MAX_CACHED_FILE_SIZE as u64 {
        return false;
    }

    let mut hole = 0;
    let r = unsafe { ext4_fseek_hole_raw(file, 0, &mut hole) };
    r != EOK as i32 || hole < size
}

fn cached_entries_for_inode(key: WholeFileCacheKey) -> Vec<(String, Arc<VFileCacheLock>)> {
    let entries: Vec<(String, Arc<VFileCacheLock>)> = CACHE_TABLE
        .lock()
        .iter()
        .map(|(path, cache)| (path.clone(), cache.clone()))
        .collect();

    entries
        .into_iter()
        .filter(|(_, cache)| cache.read().inode_key == Some(key))
        .collect()
}

fn flush_inode_caches(key: WholeFileCacheKey) -> Result<(), i32> {
    for (path, cache) in cached_entries_for_inode(key) {
        write_back_cache_entry(&path, &cache)?;
    }
    Ok(())
}

fn discard_inode_caches(key: WholeFileCacheKey) {
    for (path, _) in cached_entries_for_inode(key) {
        remove_file_cache_state(&path);
    }
}

fn flush_ext4_block_cache_for_path(path: &str) -> Result<usize, i32> {
    let c_path = CString::new(path).expect("CString::new failed");
    let c_path = c_path.into_raw();
    let r = unsafe { ext4_cache_flush(c_path) };
    unsafe {
        drop(CString::from_raw(c_path));
    }
    if r != EOK as i32 {
        error!("ext4_cache_flush: {}, rc = {}", path, r);
        return Err(r);
    }
    Ok(0)
}

pub fn if_cache(file_path: String) -> bool {
    CACHE_TABLE.lock().contains_key(&file_path)
}

/// Read a cached file at an explicit offset without touching an `Ext4File`
/// descriptor.  This is used by VFS read paths to preserve dirty write-back
/// data while avoiding the descriptor seek/cache bookkeeping on every call.
pub fn read_cached_at(path: &str, offset: usize, buff: &mut [u8]) -> Option<usize> {
    let cache = CACHE_TABLE.lock().get(path).cloned()?;
    let cache = cache.read();
    if cache.evicting {
        return None;
    }
    if offset >= cache.size {
        return Some(0);
    }
    let end = offset.saturating_add(buff.len()).min(cache.size);
    let read_size = end - offset;
    buff[..read_size].copy_from_slice(&cache.data[offset..end]);
    Some(read_size)
}

fn overlay_cached_stat(path: &str, stat: &mut ext4_inode_stat) {
    let Some(cache) = CACHE_TABLE.lock().get(path).cloned() else {
        return;
    };
    let cache = cache.read();
    if cache.evicting {
        return;
    }
    stat.st_size = cache.size as isize;
    if stat.st_blksize > 0 {
        stat.st_blocks = (stat.st_size + stat.st_blksize as isize - 1) / stat.st_blksize as isize;
    }
}

/// Move a path-keyed dense write-back cache after a successful rename.
///
/// The directory entry has already been moved by lwext4, so new opens must
/// observe the dirty bytes through the destination pathname until normal
/// eviction, fsync, or close writes them back. A failed rename must not call
/// this helper; keeping the source key then preserves retry semantics.
pub fn rename_path_cache(old_path: &str, new_path: &str) -> Option<usize> {
    if old_path == new_path {
        return CACHE_TABLE
            .lock()
            .get(new_path)
            .cloned()
            .map(|cache| cache.read().size);
    }

    let moved = {
        let mut table = CACHE_TABLE.lock();
        table.remove(new_path);
        let cache = table.remove(old_path)?;
        let size = cache.read().size;
        table.insert(String::from(new_path), cache);
        Some(size)
    };

    let mut fifo = FIFO_TABLE.lock();
    let mut had_source_fifo = false;
    fifo.retain(|entry| {
        if entry == old_path {
            had_source_fifo = true;
            false
        } else {
            entry != new_path
        }
    });
    if had_source_fifo {
        fifo.push_back(String::from(new_path));
    }
    moved
}

/// Update an existing dense write-back cache without touching an `Ext4File`.
///
/// `Some` means that a resident byte cache can represent this write exactly;
/// `None` leaves the caller on its serialized lwext4 slow path for cache
/// creation, sparse writes, and cache-size transitions.  The cache's own
/// write lock publishes the update before a FIFO eviction can write it back.
pub fn write_cached_at(path: &str, offset: usize, buf: &[u8]) -> Option<Result<usize, i32>> {
    let cache = CACHE_TABLE.lock().get(path).cloned()?;
    let mut cache_writer = cache.write();
    if cache_writer.evicting {
        return None;
    }
    let next_size = match offset.checked_add(buf.len()) {
        Some(size) => size,
        None => return Some(Err(EINVAL as i32)),
    };
    let write_creates_hole = !buf.is_empty() && offset > cache_writer.size;
    if next_size > MAX_CACHED_FILE_SIZE || write_creates_hole {
        return None;
    }

    cache_writer.offset = offset;
    if let Err(error) = cache_writer.writebuf(buf) {
        return Some(Err(error));
    }
    drop(cache_writer);
    touch_fifo_path(path);
    #[cfg(feature = "perf")]
    {
        perf::record_write_cache_hit(buf.len());
        perf::record_write_cache_fast_hit(buf.len());
    }
    Some(Ok(buf.len()))
}

fn get_cache(file_path: &str) -> Option<Arc<VFileCacheLock>> {
    CACHE_TABLE.lock().get(file_path).cloned()
}

fn insert_cache(file_path: String, cache: &Arc<VFileCacheLock>) {
    CACHE_TABLE.lock().insert(file_path, cache.clone());
}

pub fn remove_cache(file_path: String) {
    CACHE_TABLE.lock().remove(&file_path);
}

/// Drop every global write-back bookkeeping entry for a pathname without
/// writing it back. Callers must persist dirty data first when it still
/// belongs to a live directory entry. Sparse-layout policy is inode-based and
/// deliberately survives this pathname cleanup.
pub fn discard_path_cache(file_path: &str) {
    remove_file_cache_state(file_path);
}

/// Remove every global write-back bookkeeping entry for a pathname.
///
/// The guards are deliberately released between tables so cache removal never
/// waits for FIFO state while retaining a cache-table guard.
fn remove_fifo_path(file_path: &str) {
    FIFO_TABLE.lock().retain(|entry| entry != file_path);
}

fn remove_file_cache_state(file_path: &str) {
    remove_fifo_path(file_path);
    CACHE_TABLE.lock().remove(file_path);
}

fn is_proc_task_runtime_file(path: &str) -> bool {
    let rest = match path.strip_prefix("/proc/") {
        Some(rest) => rest,
        None => return false,
    };
    let (pid, name) = match rest.split_once('/') {
        Some(parts) => parts,
        None => return false,
    };

    !pid.is_empty()
        && pid.as_bytes().iter().all(|b| matches!(*b, b'0'..=b'9'))
        && matches!(name, "stat" | "status" | "maps")
}

// Cargo/rustc keeps several temporary outputs active per compiler worker.  A
// ten-entry FIFO evicts a still-growing file merely because another worker
// touched its artifact, then rebuilds and writes the whole byte cache while
// the caller holds the serialized lwext4 lock.  Keep a bounded active working
// set and promote modified entries so those files remain cache-resident.
//
// Each entry is independently capped by `MAX_CACHED_FILE_SIZE`; 32 entries
// therefore remain bounded (at most 512 MiB) on the 8/16 GiB QEMU targets.
const FIFO_SIZE: usize = 32;
static FIFO_TABLE: Lazy<Mutex<VecDeque<String>>> = Lazy::new(|| Mutex::new(VecDeque::new()));

/// Mark a write-back cache entry as recently modified.
///
/// This deliberately touches only FIFO bookkeeping.  The caller has released
/// the file-cache write lock, so an eviction may persist the completed update
/// before this promotion; that is safe because the cache lock published the
/// bytes before the eviction could acquire it.
fn touch_fifo_path(file_path: &str) {
    let mut fifo = FIFO_TABLE.lock();
    if let Some(index) = fifo.iter().position(|entry| entry == file_path) {
        let path = fifo.remove(index).expect("FIFO entry disappeared");
        fifo.push_back(path);
    }
}

fn insert_fifo(file_path: String) -> Result<(), i32> {
    loop {
        let evicted = {
            let mut fifo = FIFO_TABLE.lock();
            // FIFO_SIZE is intentionally small, so keeping de-duplication in
            // the queue itself avoids a second table with a separate lifetime.
            if fifo.iter().any(|entry| entry == &file_path) {
                return Ok(());
            }
            if fifo.len() < FIFO_SIZE {
                fifo.push_back(file_path.clone());
                return Ok(());
            }

            let path = fifo.front().cloned().expect("full FIFO has no front");
            match CACHE_TABLE.lock().get(&path).cloned() {
                Some(cache) => Some((path, cache)),
                // Repair an orphaned queue entry before selecting another
                // victim.  No cache data exists for this pathname anymore.
                None => {
                    fifo.pop_front();
                    None
                }
            }
        };

        let Some((path, cache)) = evicted else {
            continue;
        };

        // Write-back runs without either global table lock.  Leave both table
        // entries reachable until it succeeds so a transient I/O error can be
        // retried instead of silently dropping dirty user data.
        let _written = match write_back_cache_entry(&path, &cache) {
            Ok(written) => written,
            Err(r) => {
                error!("write-back cache eviction: {}, rc = {}", path, r);
                return Err(r);
            }
        };
        #[cfg(feature = "perf")]
        perf::record_write_cache_eviction(_written);

        // A write can arrive after the snapshot reached disk. Claiming a
        // still-clean entry makes those racing writers bypass the cache before
        // its table reference disappears; otherwise keep the dirty entry for
        // a later eviction rather than dropping user data.
        let removed = if cache.claim_clean_for_eviction() {
            let removed = {
                let mut table = CACHE_TABLE.lock();
                if table
                    .get(&path)
                    .is_some_and(|current| Arc::ptr_eq(current, &cache))
                {
                    table.remove(&path);
                    true
                } else {
                    false
                }
            };
            if !removed {
                // Rename/unlink may have replaced this path while write-back
                // was in flight. The cache remains reachable elsewhere, so
                // undo the transient admission barrier.
                cache.cancel_eviction();
            }
            removed
        } else {
            false
        };

        let cache_still_at_path = CACHE_TABLE
            .lock()
            .get(&path)
            .is_some_and(|current| Arc::ptr_eq(current, &cache));
        let mut fifo = FIFO_TABLE.lock();
        fifo.retain(|entry| entry != &path);
        if !removed && cache_still_at_path {
            fifo.push_back(path);
        }
    }
}

pub fn write_back_cache(path: String) -> Result<usize, i32> {
    let cache = CACHE_TABLE.lock().get(&path).cloned();
    match cache {
        Some(cache) => write_back_cache_entry(&path, &cache),
        None => Ok(0),
    }
}

fn write_back_cache_entry(path: &str, cache: &Arc<VFileCacheLock>) -> Result<usize, i32> {
    // Do not hold the data lock while entering lwext4. A write-back snapshot
    // has its own lifetime, and the flush gate orders snapshots so an older
    // I/O completion cannot overwrite a newer one on disk.
    let _flush = cache.flush.lock();
    let Some(snapshot) = cache.snapshot_for_writeback()? else {
        return Ok(0);
    };

    let c_path = CString::new(path).expect("CString::new failed");
    let flags = Ext4File::flags_to_cstring(O_RDWR);
    let mut file_desc = ext4_file {
        mp: core::ptr::null_mut(),
        inode: 0,
        flags: 0,
        fsize: 0,
        fpos: 0,
    };
    let mut r = unsafe { ext4_fopen(&mut file_desc, c_path.as_ptr(), flags.as_ptr()) };
    if r == ENOENT as i32 {
        // Runtime proc files are removed together with their task.
        // They must never be recreated from a stale write-back cache.
        if is_proc_task_runtime_file(path) {
            cache.mark_clean_if_unchanged(snapshot.revision);
            return Ok(0);
        }
        // A newly created file can remain only in the write-back cache
        // until its first eviction. Materialize that cache entry and
        // then flush the pending contents through the same descriptor.
        file_desc = ext4_file {
            mp: core::ptr::null_mut(),
            inode: 0,
            flags: 0,
            fsize: 0,
            fpos: 0,
        };
        let c_path = CString::new(path).expect("CString::new failed");
        let flags = Ext4File::flags_to_cstring(O_RDWR | O_CREAT | O_TRUNC);
        r = unsafe { ext4_fopen(&mut file_desc, c_path.as_ptr(), flags.as_ptr()) };
        if r == EOK as i32 {
            if let Some(mode) = snapshot.mode {
                let c_path = CString::new(path).expect("CString::new failed");
                r = unsafe { ext4_mode_set(c_path.as_ptr(), mode) };
                if r != EOK as i32 {
                    unsafe {
                        ext4_fclose(&mut file_desc);
                    }
                }
            }
        }
    }
    if r != EOK as i32 {
        error!("write_back_cache ext4_fopen: {}, rc = {}", path, r);
        return Err(r);
    }

    if whole_file_cache_disabled_for_desc(&file_desc) {
        // An alias may have created sparse layout after this path's cache was
        // populated. Dropping the stale byte-only cache is safer than turning
        // its holes into zero-filled allocated blocks during FIFO eviction.
        let close_r = unsafe { ext4_fclose(&mut file_desc) };
        if close_r != EOK as i32 {
            return Err(close_r);
        }
        cache.mark_clean_if_unchanged(snapshot.revision);
        return Ok(0);
    }

    file_desc.fpos = 0;
    let mut rw_count = 0;
    let r = unsafe {
        ext4_fwrite(
            &mut file_desc,
            snapshot.data.as_ptr() as _,
            snapshot.size,
            &mut rw_count,
        )
    };
    if r != EOK as i32 {
        error!("write_back_cache ext4_fwrite: {}, rc = {}", path, r);
        unsafe {
            ext4_fclose(&mut file_desc);
        }
        return Err(r);
    }
    let r = unsafe { ext4_fclose(&mut file_desc) };
    if r != EOK as i32 {
        error!("write_back_cache ext4_fclose: {}, rc = {}", path, r);
        return Err(r);
    }
    if rw_count != snapshot.size {
        error!(
            "write_back_cache short write: {}, expected {}, got {}",
            path, snapshot.size, rw_count
        );
        return Err(EIO as i32);
    }
    cache.mark_clean_if_unchanged(snapshot.revision);
    Ok(rw_count)
}

fn seek_pos(current: usize, size: usize, offset: i64, seek_type: u32) -> Result<usize, i32> {
    match seek_type {
        SEEK_SET => {
            if offset < 0 {
                Err(EINVAL as i32)
            } else {
                Ok(offset as usize)
            }
        }
        SEEK_CUR => {
            if offset < 0 {
                current.checked_sub((-offset) as usize).ok_or(EINVAL as i32)
            } else {
                current.checked_add(offset as usize).ok_or(EINVAL as i32)
            }
        }
        SEEK_END => {
            if offset < 0 {
                size.checked_sub((-offset) as usize).ok_or(EINVAL as i32)
            } else {
                size.checked_add(offset as usize).ok_or(EINVAL as i32)
            }
        }
        _ => Err(EINVAL as i32),
    }
}
