use crate::{
    fs::{Socket, files::OSFile},
    mm::UserBuffer,
    syscall::Syscall,
    utils::{GeneralRet, SysErrNo, SyscallRet},
};
use alloc::{sync::Arc, vec, vec::Vec};

use log::debug;
use spin::rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::{File, FileClass, OpenFlags, Stdin, Stdout};
pub struct FdTable {
    inner: RwLock<FdTableInner>,
}

#[derive(Clone)]
pub struct FileDescriptor {
    flags: OpenFlags,
    file: FileClass,
}

impl FileDescriptor {
    pub fn new(flags: OpenFlags, file: FileClass) -> Self {
        Self { flags, file }
    }
    pub fn default(file: FileClass) -> Self {
        Self {
            flags: OpenFlags::empty(),
            file,
        }
    }
    pub fn flags(&self) -> u32 {
        self.flags.bits()
    }
    pub fn file(&self) -> Result<Arc<OSFile>, SysErrNo> {
        self.file.file()
    }
    pub fn abs(&self) -> Result<Arc<dyn File>, SysErrNo> {
        self.file.abs()
    }
    pub fn socket(&self) ->Result<Arc<Socket>, SysErrNo> {
        self.file.socket()
    }
    pub fn any(&self) -> Arc<dyn File> {
        self.file.any()
    }

    pub fn unset_cloexec(&mut self) {
        self.flags.remove(OpenFlags::O_CLOEXEC);
    }
    pub fn set_cloexec(&mut self) {
        self.flags.insert(OpenFlags::O_CLOEXEC);
    }
    pub fn cloexec(&self) -> bool {
        self.flags.contains(OpenFlags::O_CLOEXEC)
    }
    pub fn non_block(&self) -> bool {
        self.flags.contains(OpenFlags::O_NONBLOCK)
    }
    pub fn unset_nonblock(&mut self) {
        self.flags.remove(OpenFlags::O_NONBLOCK);
    }
    pub fn set_nonblock(&mut self) {
        self.flags.insert(OpenFlags::O_NONBLOCK);
    }
}

pub struct FdTableInner {
    soft_limit: usize,
    hard_limit: usize,
    files: Vec<Option<FileDescriptor>>,
}

impl FdTableInner {
    pub fn empty() -> Self {
        Self {
            soft_limit: 128,
            hard_limit: 256,
            files: Vec::new(),
        }
    }
    pub fn new(soft_limit: usize, hard_limit: usize, files: Vec<Option<FileDescriptor>>) -> Self {
        Self {
            soft_limit,
            hard_limit,
            files,
        }
    }
}

impl FdTable {
    /// 创建新的fd_table
    fn new(fd_table: FdTableInner) -> Self {
        Self {
            inner: RwLock::new(fd_table),
        }
    }
    /// 创建带有标准输入输出错误的fd_table
    pub fn new_with_stdio() -> Self {
        FdTable::new(FdTableInner::new(
            128,
            256,
            vec![
                Some(FileDescriptor {
                    flags: OpenFlags::empty(),
                    file: FileClass::Abs(Arc::new(Stdin)),
                }),
                Some(FileDescriptor {
                    flags: OpenFlags::empty(),
                    file: FileClass::Abs(Arc::new(Stdout)),
                }),
                Some(FileDescriptor {
                    flags: OpenFlags::empty(),
                    file: FileClass::Abs(Arc::new(Stdout)),
                }),
            ],
        ))
    }
    /// 克隆一份页表
    pub fn from_another(another: &Arc<FdTable>) -> Self {
        let other = another.get_ref();
        Self {
            inner: RwLock::new(FdTableInner {
                soft_limit: other.soft_limit,
                hard_limit: other.hard_limit,
                files: other.files.clone(),
            }),
        }
    }
    /// 清空fd_table
    pub fn clear(&self) {
        self.get_mut().files.clear();
    }
    /// 分配一个新的最小可用fd
    pub fn alloc_fd(&self) -> SyscallRet {
        let mut inner = self.get_mut();
        let soft_limit = inner.soft_limit;
        let fd_table = &mut inner.files;

        if let Some(fd) = fd_table.iter().position(|slot| slot.is_none()) {
            return Ok(fd);
        }

        if fd_table.len() >= soft_limit {
            return Err(SysErrNo::EMFILE);
        }

        fd_table.push(None);
        Ok(fd_table.len() - 1)
    }
    /// 分配大于参数的fd
    pub fn alloc_fd_larger_than(&self, arg: usize) -> SyscallRet {
        let mut inner = self.get_mut();
        let soft_limit = inner.soft_limit;
        let fd_table = &mut inner.files;

        if arg > soft_limit {
            return Err(SysErrNo::EMFILE);
        }
        if fd_table.len() + 1 > soft_limit {
            return Err(SysErrNo::EMFILE);
        }
        if fd_table.len() < arg {
            fd_table.resize(arg, None);
        }
        if let Some(fd) = fd_table.iter().skip(arg).position(|slot| slot.is_none()) {
            Ok(fd + arg)
        } else {
            fd_table.push(None);
            Ok(fd_table.len() - 1)
        }
    }
    /// 对fd表中权限位有O_CLOEXEC进行关闭
    pub fn close_on_exec(&self) {
        let fd_table = &mut self.get_mut().files;
        for fd in fd_table {
            if fd.is_some() && fd.as_ref().unwrap().flags.contains(OpenFlags::O_CLOEXEC) {
                fd.take();
            }
        }
    }
    /// 返回fd表的长度
    pub fn len(&self) -> usize {
        self.get_ref().files.len()
    }
    /// 判断fd表是否为空
    pub fn is_empty(&self) -> bool {
        self.len() != 0
    }
    /// 修改fd表的大小
    pub fn resize(&self, size: usize) -> GeneralRet {
        let mut inner = self.get_mut();
        let soft_limit = inner.soft_limit;
        let fd_table = &mut inner.files;
        if size > soft_limit {
            return Err(SysErrNo::EMFILE);
        }
        fd_table.resize(size, None);
        Ok(())
    }

    /// 安全地尝试获取文件描述符副本
    pub fn try_get(&self, fd: usize) -> Option<FileDescriptor> {
        self.get_ref()
            .files
            .get(fd) // 安全检查边界
            .and_then(|x| x.as_ref()) // 检查是否为 Some
            .cloned()
    }

    /// 用于系统调用：获取文件描述符，失败返回 EBADF
    pub fn get(&self, fd: usize) -> Result<FileDescriptor, SysErrNo> {
        self.try_get(fd).ok_or(SysErrNo::EBADF)
    }

    pub fn get_cloexec(&self, fd: usize) -> Result<bool, SysErrNo> {
        let fd_inner = self.get_ref();
        let desc = fd_inner
            .files
            .get(fd)
            .and_then(|x| x.as_ref())
            .ok_or(SysErrNo::EBADF)?;
        Ok(desc.flags.contains(OpenFlags::O_CLOEXEC))
    }

    pub fn set_cloexec(&self, fd: usize) -> SyscallRet {
        let mut inner = self.get_mut();
        let file_desc = inner
            .files
            .get_mut(fd)
            .and_then(|slot| slot.as_mut())
            .ok_or(SysErrNo::EBADF)?;

        file_desc.flags.insert(OpenFlags::O_CLOEXEC);
        Ok(0)
    }

    pub fn unset_cloexec(&self, fd: usize) -> SyscallRet {
        let mut inner = self.get_mut();
        let desc = inner
            .files
            .get_mut(fd)
            .and_then(|x| x.as_mut())
            .ok_or(SysErrNo::EBADF)?;
        desc.flags.remove(OpenFlags::O_CLOEXEC);
        Ok(0)
    }

    pub fn set_nonblock(&self, fd: usize) -> SyscallRet {
        let mut inner = self.get_mut();
        let desc = inner
            .files
            .get_mut(fd)
            .and_then(|x| x.as_mut())
            .ok_or(SysErrNo::EBADF)?;
        desc.flags.insert(OpenFlags::O_NONBLOCK);
        Ok(0)
    }

    pub fn unset_nonblock(&self, fd: usize) -> SyscallRet {
        let mut inner = self.get_mut();
        let desc = inner
            .files
            .get_mut(fd)
            .and_then(|x| x.as_mut())
            .ok_or(SysErrNo::EBADF)?;
        desc.flags.remove(OpenFlags::O_NONBLOCK);
        Ok(0)
    }

    pub fn get_hard_limit(&self) -> usize {
        self.get_ref().hard_limit
    }

    pub fn get_soft_limit(&self) -> usize {
        self.get_ref().soft_limit
    }

    pub fn set_limit(&self, soft_limit: usize, hard_limit: usize) {
        let mut inner = self.get_mut();
        inner.soft_limit = soft_limit;
        inner.hard_limit = hard_limit;
    }

    pub fn set(&self, fd: usize, file: FileDescriptor) -> Result<(), SysErrNo> {
        let mut inner = self.get_mut();
        if fd >= inner.soft_limit {
            return Err(SysErrNo::EMFILE);
        }
        if fd >= inner.files.len() {
            inner.files.resize(fd + 1, None);
        }
        inner.files[fd] = Some(file);
        Ok(())
    }
    /// 修改文件描述符
    pub fn set_flags(&self, fd: usize, file: FileDescriptor) {
        self.get_mut().files[fd] = Some(file);
    }

    pub fn take(&self, fd: usize) -> Option<FileDescriptor> {
        let mut inner = self.get_mut();
        inner.files.get_mut(fd).and_then(|slot| slot.take())
    }

    fn get_mut(&self) -> RwLockWriteGuard<'_, FdTableInner> {
        self.inner.write()
    }

    fn get_ref(&self) -> RwLockReadGuard<'_, FdTableInner> {
        self.inner.read()
    }
    pub fn with_fd_mut<F, T>(&self, fd: usize, mut f: F) -> Result<T, SysErrNo>
    where
        F: FnMut(&mut FileDescriptor) -> T,
    {
        let mut inner = self.get_mut();
        if let Some(Some(fd_obj)) = inner.files.get_mut(fd) {
            Ok(f(fd_obj))
        } else {
            Err(SysErrNo::EBADFD)
        }
    }
    // 下面的函数要求直接对文件进行操作而不只是flags

    pub fn try_get_file(&self, fd: usize) -> Option<Arc<dyn File>> {
        self.get_mut().files[fd].as_mut().map(|f| f.any())
    }
}
