// 该文件定义了抽象的Inode trait和File trait

use core::task::Context;

use super::{InodeType, Kstat, Statfs};
use crate::{
    fs::{OpenFlags, String},
    mm::UserBuffer,
    syscall::PollEvents,
    utils::{SysErrNo, SysResult, SyscallRet},
};
use alloc::{borrow::Cow, sync::Arc, vec::Vec};

/// fcntl(F_SETOWN/F_SETOWN_EX/F_SETSIG) 使用的异步 I/O 信号目标。
#[derive(Clone, Copy, Debug)]
pub struct FasyncOwner {
    pub owner_type: i32,
    pub pid: i32,
    pub signal: i32,
}

impl Default for FasyncOwner {
    fn default() -> Self {
        Self {
            owner_type: 1,
            pid: 0,
            signal: 0,
        }
    }
}

/// 超级块抽象
pub trait SuperBlock: Send + Sync {
    fn root_inode(&self) -> Arc<dyn Inode>;
    fn sync(&self);
    fn fs_stat(&self) -> Statfs;
    fn ls(&self);
}
/// VfsInode接口
/// 但是，在本项目中，它实际上只可能是Ext4Inode...
pub trait Inode: Send + Sync {
    /// 返回inode的大小
    fn size(&self) -> usize {
        unimplemented!("Inode::size")
    }
    /// 返回inode的大小
    fn types(&self) -> InodeType {
        unimplemented!("inode::types")
    }
    /// 返回inode的元数据，依据Kstate的字段
    fn fstat(&self) -> Kstat {
        unimplemented!("Inode::fstat")
    }
    /// Mark the cached metadata of this inode's directory as changed.
    ///
    /// Filesystem backends with a directory-local metadata cache may override
    /// this hook when a namespace operation identifies the affected parent.
    /// Other backends retain the no-op default and their existing invalidation
    /// strategy.
    fn mark_directory_stat_changed(&self) {}
    /// 在当前目录下创建文件或目录
    fn create(&self, _path: &str, _ty: InodeType) -> Result<Arc<dyn Inode>, SysErrNo> {
        unimplemented!("Inode::create")
    }
    /// Create a node and apply the metadata assigned by `open(O_CREAT)`.
    ///
    /// Filesystems with a mount-wide operation lock may override this to keep
    /// the create/mode/owner sequence in one critical section.  The default
    /// preserves the existing separate operations for other backends.
    fn create_with_metadata(
        &self,
        path: &str,
        ty: InodeType,
        mode: u32,
        owner: Option<(u32, u32)>,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        let inode = self.create(path, ty)?;
        inode.fmode_set(mode)?;
        if let Some((uid, gid)) = owner {
            inode.owner_set(uid, gid)?;
        }
        Ok(inode)
    }
    /// 创建一个内核已知不存在的目录，绕过通用 open(O_CREATE) 语义。
    ///
    /// 该入口只用于 proc 等内核维护的固定目录：调用者已经完成父目录解析，
    /// 不需要再次执行权限、umask、owner 和普通文件描述符处理。
    fn create_dir_fast(&self, path: &str) -> Result<Arc<dyn Inode>, SysErrNo> {
        self.create(path, InodeType::Dir)
    }
    /// 查找文件
    fn find(
        &self,
        _path: &str,
        _flags: OpenFlags,
        _loop_times: usize,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        unimplemented!("Inode::find")
    }
    /// Resolve one final directory entry below an already-cached parent.
    ///
    /// Backends that need a full pathname fallback can override this to skip
    /// work which the cached, concrete parent inode has already ruled out.
    /// The default retains the complete `find()` behavior for other filesystems.
    fn find_from_cached_parent(
        &self,
        path: &str,
        flags: OpenFlags,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        self.find(path, flags, 0)
    }
    /// 在指定偏移位置读取数据进buf
    fn read_at(&self, _off: usize, _buf: &mut [u8]) -> SyscallRet {
        unimplemented!("Inode::read_at")
    }
    /// Record a successful file access.
    fn touch_atime(&self) -> SyscallRet {
        Ok(0)
    }
    /// 在指定偏移位置将buf的数据写入
    fn write_at(&self, _off: usize, _buf: &[u8]) -> SyscallRet {
        unimplemented!("Inode::write_at")
    }
    /// 读取目录项
    fn read_dentry(&self, _off: usize, _len: usize) -> Result<(Vec<u8>, isize), SysErrNo> {
        unimplemented!("Inode::read_dentry")
    }
    /// 判断目录是否为空。用于 rmdir/unlinkat(AT_REMOVEDIR) 在调用底层删除前保持
    /// Linux 语义，避免文件系统后端执行递归删除。
    fn is_dir_empty(&self) -> Result<bool, SysErrNo> {
        unimplemented!("Inode::is_dir_empty")
    }
    /// 截断文件到指定大小
    fn truncate(&self, _size: usize) -> SyscallRet {
        unimplemented!("Inode::truncate")
    }
    /// 同步文件状态
    fn sync(&self) {
        unimplemented!("Inode::sync")
    }
    /// 设置文件时间
    fn set_timestamps(
        &self,
        _atime: Option<u64>,
        _mtime: Option<u64>,
        _ctime: Option<u64>,
    ) -> SyscallRet {
        unimplemented!("Inode::set_timestamps")
    }
    /// Set one extended attribute on this inode.
    ///
    /// `name` excludes the terminating NUL. `flags` uses the Linux
    /// `XATTR_CREATE` / `XATTR_REPLACE` bits and is validated by the backend
    /// while it holds the filesystem operation lock.
    fn set_xattr(&self, _name: &[u8], _value: &[u8], _flags: u32) -> SyscallRet {
        Err(SysErrNo::EOPNOTSUPP)
    }
    /// Read one extended attribute. An empty buffer queries the value length.
    fn get_xattr(&self, _name: &[u8], _value: &mut [u8]) -> SyscallRet {
        Err(SysErrNo::EOPNOTSUPP)
    }
    /// Return the NUL-separated extended-attribute name list.
    fn list_xattr(&self, _list: &mut [u8]) -> SyscallRet {
        Err(SysErrNo::EOPNOTSUPP)
    }
    /// Remove one extended attribute from this inode.
    fn remove_xattr(&self, _name: &[u8]) -> SyscallRet {
        Err(SysErrNo::EOPNOTSUPP)
    }
    fn link_cnt(&self) -> SyscallRet {
        unimplemented!("Inode::link_cnt")
    }
    fn unlink(&self, _path: &str) -> SyscallRet {
        unimplemented!("Inode::unlink");
    }
    fn read_link(&self, _buf: &mut [u8], _bufsize: usize) -> SyscallRet {
        unimplemented!("Inode::read_link")
    }
    fn sym_link(&self, _target: &str, _path: &str) -> SyscallRet {
        unimplemented!("Inode::sym_link")
    }
    fn rename(&self, _path: &str, _new_path: &str) -> SyscallRet {
        unimplemented!("Inode::rename")
    }
    /// 创建硬链接
    fn hard_link(&self, _old_path: &str, _new_path: &str) -> SyscallRet {
        unimplemented!("Inode::hard_link")
    }
    fn delay(&self) {
        unimplemented!("Inode::delay")
    }
    fn read_all(&self) -> Result<Vec<u8>, SysErrNo> {
        unimplemented!("Inode::read_all");
    }
    /// 返回 inode 当前 VFS 路径的独立快照。
    ///
    /// 这是通用路径接口。调用者可以长期持有、修改返回的 `String`，或将其传给
    /// 基于路径的文件系统 API，而无需继续借用 inode。若后端以
    /// `RwLock<Arc<str>>` 保存路径，实现通常会短暂取得读锁，然后分配新的
    /// `String` 并复制全部路径字节，最后释放锁。
    ///
    /// 当调用方需要独立拥有的路径，或后端不能提供可共享的缓存键时使用此方法。
    /// 若 `page_cache_path()` 可用，不要在文件页缓存的高频查询中反复调用本方法：
    /// 每次调用都可能为较长路径分配内存并复制字节。
    fn path(&self) -> String {
        unimplemented!("Inode::path");
    }
    /// 返回供文件页缓存查询共享使用的路径。
    ///
    /// 这是缓存键优化接口，不是通用路径接口。后端若以 `Arc<str>` 保存当前 VFS
    /// 路径，可返回 `Some(arc.clone())`：clone 只增加 `Arc` 的引用计数，既不分配
    /// 内存，也不复制路径字节。调用方拥有返回的 `Arc`，而不是持锁的 `&str` 引用，
    /// 因此后端可以在页缓存查询前释放内部锁。
    ///
    /// `path()` 和 `page_cache_path()` 可以读取同一个内部路径字段，但所有权与开销
    /// 不同：
    ///
    /// - `path()` 返回包含路径字节完整副本的新 `String`。
    /// - `page_cache_path()` 返回共享相同路径字节的 `Arc<str>` clone。
    ///
    /// 仅当返回值与 [`FilePageCache`](crate::fs::FilePageCache) 的键完全一致，且后端
    /// 会在 VFS 路径变化时同步更新它，才应返回 `Some`。路径并非 inode 身份：rename、
    /// unlink 后重建、truncate 和写入仍须遵守已有的页缓存失效规则。默认返回 `None`
    /// 以兼容未维护共享路径的后端；调用方此时必须回退到 `path()`。
    fn page_cache_path(&self) -> Option<Arc<str>> {
        None
    }
    /// 返回底层文件系统在本 inode 生命周期内稳定的身份键。
    ///
    /// 路径查找已经取得 `(st_dev, st_ino)` 时，VFS inode 索引可直接复用它，
    /// 不必为建立缓存再执行一次可能串行的 `fstat()`。后端无法可靠提供该
    /// 身份时返回 `None`，索引会保留原来的 `fstat()` 回退路径。
    fn cache_identity(&self) -> Option<(usize, usize)> {
        None
    }
    /// Returns whether the immutable identity captured during lookup is still
    /// usable for a cache-key collision without a live metadata probe.
    ///
    /// Backends must return `false` unless they can prove that no successful
    /// namespace operation capable of recycling an inode number occurred
    /// since the identity was captured.  `FsIndex` then retains its existing
    /// `fstat()` validation fallback for every uncertain case.
    fn cache_identity_is_current(&self) -> bool {
        false
    }
    /// Mark a freshly constructed candidate as following an actual FsIndex
    /// reclaim. Backends with a stat cache can retain this as the next miss
    /// cause without evicting already valid metadata.
    #[cfg(feature = "perf")]
    fn mark_fstat_cache_fsidx_rebuild(&self) {}
    /// 记录该 inode 的一个可用路径别名。
    ///
    /// 真正的 inode cache 会让硬链接等多个路径复用同一个 inode 对象。
    /// 默认实现为空，路径敏感的具体文件系统可以用它维护底层路径别名。
    fn cache_path_alias(&self, _path: &str) {}
    ///获取文件的mode，遇到需要文件访问权限的需要使用，暂时放在这里
    fn fmode(&self) -> Result<u32, SysErrNo> {
        unimplemented!("Inode:;fmode");
    }
    fn fmode_set(&self, _mode: u32) -> SyscallRet {
        unimplemented!("Inode::fmode_set")
    }
    /// Update inode uid/gid as visible through stat/fstat.
    fn owner_set(&self, _uid: u32, _gid: u32) -> SyscallRet {
        unimplemented!("Inode::owner_set")
    }
    /// SEEK_DATA: find next data offset >= `offset` (in bytes).
    /// Returns the byte offset, or ENXIO if no data beyond offset.
    fn seek_data(&self, _offset: usize) -> SyscallRet {
        Err(SysErrNo::ENXIO)
    }
    /// SEEK_HOLE: find next hole offset >= `offset` (in bytes).
    /// Returns the byte offset, or the file size (implicit hole at EOF).
    fn seek_hole(&self, _offset: usize) -> SyscallRet {
        Err(SysErrNo::ENXIO)
    }
}

/// 文件接口
pub trait File: Send + Sync {
    fn update_signal_mask(&self, _mask: crate::signal::SigSet) -> bool {
        false
    }
    /// 默认: 可读（子类型可按需覆写）
    fn readable(&self) -> bool {
        true
    }
    /// 默认: 可写（子类型可按需覆写）
    fn writable(&self) -> bool {
        true
    }
    /// read 指的是从文件中读取数据放到缓冲区中，最多将缓冲区填满，并返回实际读取的字节数
    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        unimplemented!("File::read")
    }
    /// 将缓冲区中的数据写入文件，最多将缓冲区中的数据全部写入，并返回直接写入的字节数
    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        unimplemented!("File::wirte")
    }
    /// Resize a regular file. Abstract in-memory files may override this.
    fn truncate(&self, _size: usize) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }
    /// 内核内部向抽象文件注入数据，默认不支持。
    fn write_kernel_bytes(&self, _buf: &[u8]) -> SyscallRet {
        Err(SysErrNo::EOPNOTSUPP)
    }
    /// 获得文件信息
    fn fstat(&self) -> Kstat {
        unimplemented!("File::fstate")
    }
    /// 获取文件路径
    fn path(&self) -> Cow<'_, str> {
        unimplemented!("File::path");
    }
    /// 设置偏移量,并非所有文件都支持
    fn lseek(&self, _offset: isize, _whence: usize) -> SyscallRet {
        Err(SysErrNo::ESPIPE)
    }
    /// 是否是非阻塞的
    fn nonblocking(&self) -> bool {
        false
    }
    /// 设置为非阻塞
    fn set_nonblocking(&self, _nonblocking: bool) -> SysResult {
        Ok(())
    }
    /// ppoll处理
    fn poll(&self, _events: PollEvents) -> PollEvents {
        unimplemented!("File::poll")
    }
    /// ioctl 处理，默认返回 ENOTTY
    fn ioctl(&self, _cmd: u32, _arg: usize, _memory_set: &crate::mm::MemorySet) -> SyscallRet {
        Err(SysErrNo::ENOTTY)
    }
    /// Registers wakers for I/O events.
    fn register(&self, _context: &mut Context<'_>, _events: PollEvents) {
        unimplemented!("File::register");
    }
    /// 设置异步 I/O 信号 owner。默认实现接受但不保存，具体文件可覆写。
    fn set_fasync_owner(&self, _owner: FasyncOwner) -> SysResult {
        Ok(())
    }
    /// 获取异步 I/O 信号 owner。默认无 owner。
    fn fasync_owner(&self) -> FasyncOwner {
        FasyncOwner::default()
    }
}
