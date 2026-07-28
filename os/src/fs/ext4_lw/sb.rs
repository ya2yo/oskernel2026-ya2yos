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
use log::{debug, error, warn};
use lwext4_rust::{Ext4BlockWrapper, InodeTypes, KernelDevOp};
use spin::Lazy;

use super::{Ext4Inode, EXT4_OP_LOCK};

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
/// [`Disk`] 顺序读写接口转换成 lwext4 期望的 `read/write/seek/flush` 形式。
impl KernelDevOp for Disk {
    //type DevType = Box<Disk>;
    type DevType = Disk;

    /// 从当前设备位置读取数据，尽量填满调用方提供的缓冲区。
    ///
    /// 底层 `read_one()` 可能一次只返回部分数据，因此这里循环推进 slice。
    fn read(dev: &mut Disk, mut buf: &mut [u8]) -> Result<usize, i32> {
        //debug!("READ block device buf={}", buf.len());
        let mut read_len = 0;
        while !buf.is_empty() {
            match dev.read_one(buf) {
                Ok(0) => break,
                Ok(n) => {
                    let tmp = buf;
                    buf = &mut tmp[n..];
                    read_len += n;
                }
                Err(_e) => return Err(-1),
            }
        }
        //debug!("READ rt len={}", read_len);
        Ok(read_len)
    }

    /// 从当前设备位置写入数据，尽量写完整个缓冲区。
    fn write(dev: &mut Self::DevType, mut buf: &[u8]) -> Result<usize, i32> {
        //debug!("WRITE block device buf={}", buf.len());
        let mut write_len = 0;
        while !buf.is_empty() {
            match dev.write_one(buf) {
                Ok(0) => break,
                Ok(n) => {
                    buf = &buf[n..];
                    write_len += n;
                }
                Err(_e) => return Err(-1),
            }
        }
        //debug!("WRITE rt len={}", write_len);
        Ok(write_len)
    }
    /// 刷新设备缓存。
    ///
    /// 当前 `Disk` 抽象没有额外的 host-side flush 语义，lwext4 层面的同步由
    /// `Ext4BlockWrapper::sync()` 负责，因此这里返回成功。
    fn flush(_dev: &mut Self::DevType) -> Result<usize, i32> {
        Ok(0)
    }

    /// 调整块设备读写位置。
    ///
    /// lwext4 使用 C 风格 `SEEK_SET/SEEK_CUR/SEEK_END`，这里转换为 `Disk` 内部
    /// 的 byte offset。越界 seek 会记录 warning，但仍更新位置，保持与底层接口兼容。
    fn seek(dev: &mut Disk, off: i64, whence: i32) -> Result<i64, i32> {
        let size = dev.size();
        let new_pos = match whence as u32 {
            lwext4_rust::bindings::SEEK_SET => Some(off),
            lwext4_rust::bindings::SEEK_CUR => dev
                .position()
                .checked_add_signed(off as isize)
                .map(|v| v as i64),
            lwext4_rust::bindings::SEEK_END => {
                size.checked_add_signed(off as isize).map(|v| v as i64)
            }
            _ => {
                error!("invalid seek() whence: {}", whence);
                Some(off)
            }
        }
        .ok_or(-1)?;

        if new_pos as usize > size {
            warn!("Seek beyond the end of the block device");
        }
        dev.set_position(new_pos as usize);
        // debug!("new_pos={}", new_pos);
        Ok(new_pos)
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
