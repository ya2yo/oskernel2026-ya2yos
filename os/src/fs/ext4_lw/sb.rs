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

use super::Ext4Inode;

/// EXT4 超级块结构体，维护文件系统的全局元数据
struct Ext4SuperBlock {
    /// 包装了 lwext4 的挂载点信息，SyncUnsafeCell 用于处理 C 库的内部可变性
    inner: SyncUnsafeCell<Ext4BlockWrapper<Disk>>,
    /// 根目录节点
    root: Arc<dyn Inode>,
}

unsafe impl Send for Ext4SuperBlock {}
unsafe impl Sync for Ext4SuperBlock {}

impl SuperBlock for Ext4SuperBlock {
    /// 获取文件系统的根目录 Inode
    fn root_inode(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }

    /// 获取文件系统状态（总容量、剩余容量、块大小等）
    fn fs_stat(&self) -> Statfs {
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

    /// 将内存中的文件系统缓存同步回磁盘
    fn sync(&self) {
        self.inner.get_unchecked_mut().sync();
    }

    /// 调试用：列出文件系统根目录下的内容
    fn ls(&self) {
        self.inner
            .get_unchecked_ref()
            .lwext4_dir_ls()
            .into_iter()
            .for_each(|s| println!("{}", s));
    }
}

impl Ext4SuperBlock {
    /// 初始化超级块并挂载磁盘
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

/// 核心：为磁盘驱动实现 KernelDevOp 接口
/// 这样 lwext4 库就可以通过这些方法访问物理磁盘
impl KernelDevOp for Disk {
    //type DevType = Box<Disk>;
    type DevType = Disk;

    /// 封装磁盘读取逻辑，确保填满缓冲区
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

    /// 封装磁盘写入逻辑
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
    fn flush(_dev: &mut Self::DevType) -> Result<usize, i32> {
        Ok(0)
    }

    /// 磁盘指针定位，支持从起始、当前位置、末尾进行偏移
    fn seek(dev: &mut Disk, off: i64, whence: i32) -> Result<i64, i32> {
        let size = dev.size();
        debug!(
            "SEEK block device size:{}, pos:{}, offset={}, whence={}",
            size,
            &dev.position(),
            off,
            whence
        );
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

/// 全局静态超级块实例，Lazy 保证在第一次访问时初始化磁盘
static SUPER_BLOCK: Lazy<Arc<dyn SuperBlock>> = Lazy::new(|| {
    Arc::new(Ext4SuperBlock::new(
        Disk::new(BlockDeviceImpl::new_device()),
    ))
});

// --- 公共导出接口，简化外部模块调用 ---

pub fn superblock_root_inode() -> Arc<dyn Inode> {
    SUPER_BLOCK.root_inode()
}

pub fn superblock_sync() {
    SUPER_BLOCK.sync()
}

pub fn superblock_fs_stat() -> Statfs {
    SUPER_BLOCK.fs_stat()
}

pub fn superblock_ls() {
    SUPER_BLOCK.ls()
}
