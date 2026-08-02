//! EXT4 superblock and block-device adapter.
//!
//! 本文件负责把 Ya2yOS 的块设备 [`Disk`] 接入 `lwext4_rust`，并把挂载后的
//! EXT4 文件系统暴露为 VFS [`SuperBlock`]。文件系统主体逻辑仍由 lwext4 完成；
//! 这里主要维护全局挂载状态、根 inode，以及 lwext4 读写块设备所需的回调。

#![allow(non_snake_case)]
use crate::{
    drivers::{BlockDeviceImpl, Disk},
    fs::{Inode, Statfs, SuperBlock},
    sync::SyncUnsafeCell,
};
use alloc::sync::Arc;
use core::{
    ffi::{c_int, c_void},
    future::poll_fn,
    sync::atomic::{AtomicI32, Ordering},
    task::Poll,
};
use log::debug;
use lwext4_rust::{Ext4BlockWrapper, InodeTypes, KernelDevOp};
use spin::Lazy;

use super::{Ext4Inode, EXT4_OP_LOCK};
use crate::utils::PollSet;

static EXT4_BCACHE_WAITERS: PollSet = PollSet::new();

unsafe extern "C" fn wait_for_bcache_state(
    _ctx: *mut c_void,
    flags: *const c_int,
    mask: c_int,
    _lba: u64,
) -> c_int {
    if flags.is_null() {
        return lwext4_rust::bindings::EIO as c_int;
    }

    let flags = unsafe { &*(flags.cast::<AtomicI32>()) };
    let is_ready = || flags.load(Ordering::Acquire) & mask == 0;
    if is_ready() {
        return lwext4_rust::bindings::EOK as c_int;
    }

    if crate::task::current_task().is_none() {
        while !is_ready() {
            core::hint::spin_loop();
        }
    } else {
        crate::task::block_on(poll_fn(|cx| {
            if is_ready() {
                return Poll::Ready(());
            }

            EXT4_BCACHE_WAITERS.register(cx.waker());
            if is_ready() {
                EXT4_BCACHE_WAITERS.unregister(cx.waker());
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }));
    }

    lwext4_rust::bindings::EOK as c_int
}

unsafe extern "C" fn wake_bcache_waiters(_ctx: *mut c_void, _lba: u64) {
    EXT4_BCACHE_WAITERS.wake();
}

/// EXT4 超级块结构体，维护文件系统的全局元数据。
///
/// 当前内核只挂载一个主 EXT4 文件系统，因此超级块以全局 `Lazy` 单例形式存在。
/// `Ext4BlockWrapper` 内部封装 lwext4 的挂载点与块缓存，`root` 则把根目录暴露成
/// VFS `Inode`，供路径解析从 `/` 开始。
struct Ext4SuperBlock {
    /// 包装 lwext4 的挂载点信息。
    ///
    /// lwext4 wrapper 的许多操作需要内部可变性；这里用 `SyncUnsafeCell` 放在
    /// `SuperBlock` 后面，调用方需保证上层 VFS/单核执行路径不会并发破坏状态。
    inner: SyncUnsafeCell<Ext4BlockWrapper<Disk>>,
    /// 根目录 inode，所有绝对路径查找都从这里开始。
    root: Arc<dyn Inode>,
}

unsafe impl Send for Ext4SuperBlock {}
unsafe impl Sync for Ext4SuperBlock {}

impl SuperBlock for Ext4SuperBlock {
    /// 获取文件系统根目录 inode。
    fn root_inode(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }

    /// 获取文件系统状态（总容量、剩余容量、块大小等）。
    ///
    /// Linux `statfs(2)` 可见字段主要来自 lwext4 的 mount-point 统计信息。
    fn fs_stat(&self) -> Statfs {
        let _ext4 = EXT4_OP_LOCK.lock_for_metadata();
        let stat = self.inner.get_unchecked_ref().get_lwext4_mp_stats();
        Statfs {
            f_type: 0xEF53,
            f_bsize: stat.block_size as i64,
            f_blocks: stat.blocks_count as i64,
            f_bfree: stat.free_blocks_count as i64,
            f_bavail: stat.free_blocks_count as i64,
            f_files: stat.inodes_count as i64,
            f_ffree: stat.free_inodes_count as i64,
            f_name_len: 255,
            ..Default::default()
        }
    }

    /// 将 lwext4 内部缓存同步回磁盘。
    fn sync(&self) {
        let _ext4 = EXT4_OP_LOCK.lock_for_sync();
        self.inner.get_unchecked_mut().sync();
    }

    /// 调试用：列出文件系统根目录下的内容。
    fn ls(&self) {
        let _ext4 = EXT4_OP_LOCK.lock_for_metadata();
        self.inner
            .get_unchecked_ref()
            .lwext4_dir_ls()
            .into_iter()
            .for_each(|s| println!("{}", s));
    }
}

impl Ext4SuperBlock {
    /// 初始化超级块并挂载磁盘。
    ///
    /// `Ext4BlockWrapper::new()` 会完成 lwext4 mount 初始化；成功后创建根目录
    /// `Ext4Inode`，作为 VFS 对外的根 inode。
    pub fn new(disk: Disk) -> Self {
        // 初始化底层 lwext4 库
        let inner =
            Ext4BlockWrapper::<Disk>::new(disk).expect("failed to initialize EXT4 filesystem");
        inner.setup_bcache_sync(
            core::ptr::null_mut(),
            Some(wait_for_bcache_state),
            Some(wake_bcache_waiters),
        );
        #[cfg(feature = "perf")]
        {
            crate::utils::perf::enable_ext4_block_device_perf();
            lwext4_rust::perf::enable_bcache_perf();
        }
        // 创建根目录对象
        let root = Arc::new(Ext4Inode::new("/", InodeTypes::EXT4_DE_DIR));
        Self {
            inner: SyncUnsafeCell::new(inner),
            root,
        }
    }
}

/// 为 Ya2yOS 磁盘驱动实现 lwext4 所需的块设备操作。
///
/// `lwext4_rust` 通过 [`KernelDevOp`] 回调读写底层设备。这里把 Ya2yOS 的
/// [`Disk`] 的按位置 I/O 接口转换成 lwext4 的块请求。
///
/// 每个 lwext4 `bread`/`bwrite` 都带着自己的 LBA；这里不会读取或更新任何
/// 全局 cursor。设备层负责让完整请求（含非对齐 RMW）原子提交。
impl KernelDevOp for Disk {
    type DevType = Disk;

    fn device_size(dev: &Self::DevType) -> Result<u64, i32> {
        u64::try_from(dev.size()).map_err(|_| -1)
    }

    fn read_at(dev: &Self::DevType, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let offset = usize::try_from(offset).map_err(|_| -1)?;
        dev.read_at(offset, buf).map_err(|_| -1)
    }

    fn write_at(dev: &Self::DevType, offset: u64, buf: &[u8]) -> Result<usize, i32> {
        let offset = usize::try_from(offset).map_err(|_| -1)?;
        dev.write_at(offset, buf).map_err(|_| -1)
    }

    fn flush(dev: &Self::DevType) -> Result<usize, i32> {
        dev.flush().map_err(|_| -1)?;
        Ok(0)
    }
}

/// 全局静态超级块实例。
///
/// `Lazy` 保证第一次访问文件系统时才初始化块设备并挂载 EXT4。
static SUPER_BLOCK: Lazy<Arc<dyn SuperBlock>> = Lazy::new(|| {
    Arc::new(Ext4SuperBlock::new(
        Disk::new(BlockDeviceImpl::new_device()),
    ))
});

// --- 公共导出接口，简化外部模块调用 ---

/// 返回全局 EXT4 文件系统的根 inode。
pub fn superblock_root_inode() -> Arc<dyn Inode> {
    SUPER_BLOCK.root_inode()
}

/// 同步全局 EXT4 文件系统缓存。
pub fn superblock_sync() {
    SUPER_BLOCK.sync()
}

/// 获取全局 EXT4 文件系统的 `statfs` 信息。
pub fn superblock_fs_stat() -> Statfs {
    SUPER_BLOCK.fs_stat()
}

/// 打印全局 EXT4 文件系统根目录内容，主要用于启动期和调试输出。
pub fn superblock_ls() {
    SUPER_BLOCK.ls()
}
