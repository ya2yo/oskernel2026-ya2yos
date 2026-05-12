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
        unimplemented!()
    }
    /// 返回inode的大小
    fn types(&self) -> InodeType {
        unimplemented!()
    }
    /// 返回inode的元数据，依据Kstate的字段
    fn fstat(&self) -> Kstat {
        unimplemented!()
    }
    /// 在当前目录下创建文件或目录
    fn create(&self, _path: &str, _ty: InodeType) -> Result<Arc<dyn Inode>, SysErrNo> {
        unimplemented!()
    }
    /// 查找文件
    fn find(
        &self,
        _path: &str,
        _flags: OpenFlags,
        _loop_times: usize,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        unimplemented!()
    }
    /// 在指定偏移位置读取数据进buf
    fn read_at(&self, _off: usize, _buf: &mut [u8]) -> SyscallRet {
        unimplemented!()
    }
    /// 在指定偏移位置将buf的数据写入
    fn write_at(&self, _off: usize, _buf: &[u8]) -> SyscallRet {
        unimplemented!()
    }
    /// 读取目录项
    fn read_dentry(&self, _off: usize, _len: usize) -> Result<(Vec<u8>, isize), SysErrNo> {
        unimplemented!()
    }
    /// 截断文件到指定大小
    fn truncate(&self, _size: usize) -> SyscallRet {
        unimplemented!()
    }
    /// 同步文件状态
    fn sync(&self) {
        unimplemented!()
    }
    /// 设置文件时间
    fn set_timestamps(
        &self,
        _atime: Option<u64>,
        _mtime: Option<u64>,
        _ctime: Option<u64>,
    ) -> SyscallRet {
        unimplemented!()
    }
    fn link_cnt(&self) -> SyscallRet {
        unimplemented!()
    }
    fn unlink(&self, _path: &str) -> SyscallRet {
        unimplemented!();
    }
    fn read_link(&self, _buf: &mut [u8], _bufsize: usize) -> SyscallRet {
        unimplemented!()
    }
    fn sym_link(&self, _target: &str, _path: &str) -> SyscallRet {
        unimplemented!()
    }
    fn rename(&self, _path: &str, _new_path: &str) -> SyscallRet {
        unimplemented!()
    }
    fn delay(&self) {
        unimplemented!()
    }
    fn read_all(&self) -> Result<Vec<u8>, SysErrNo> {
        unimplemented!();
    }
    fn path(&self) -> String {
        unimplemented!();
    }
    ///获取文件的mode，遇到需要文件访问权限的需要使用，暂时放在这里
    fn fmode(&self) -> Result<u32, SysErrNo> {
        unimplemented!();
    }
    fn fmode_set(&self, _mode: u32) -> SyscallRet {
        unimplemented!()
    }
}

/// 文件接口
pub trait File: Send + Sync {
    fn readable(&self) -> bool {
        unimplemented!()
    }
    fn writable(&self) -> bool {
        unimplemented!()
    }
    /// read 指的是从文件中读取数据放到缓冲区中，最多将缓冲区填满，并返回实际读取的字节数
    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        unimplemented!()
    }
    /// 将缓冲区中的数据写入文件，最多将缓冲区中的数据全部写入，并返回直接写入的字节数
    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        unimplemented!()
    }
    /// 获得文件信息
    fn fstat(&self) -> Kstat {
        unimplemented!("not implemented!")
    }
    /// 获取文件路径
    fn path(&self) -> Cow<'_, str> {
        unimplemented!("not implemented");
    }
    /// 设置偏移量,并非所有文件都支持
    fn lseek(&self, _offset: isize, _whence: usize) -> SyscallRet {
        unimplemented!("not support!");
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
        unimplemented!()
    }
    /// Registers wakers for I/O events.
    fn register(&self, _context: &mut Context<'_>, _events: PollEvents) {
        unimplemented!("not support!");
    }
}
