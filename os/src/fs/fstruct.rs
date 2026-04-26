use crate::{
    fs::files::Socket, sync::SyncUnsafeCell, utils::{GeneralRet, SysErrNo, SyscallRet}
};
use alloc::{sync::Arc, vec, vec::Vec};

use log::debug;

use super::{File, FileClass, OSFile, OpenFlags, Stdin, Stdout};
pub struct FdTable {
    inner: SyncUnsafeCell<FdTableInner>,
}

#[derive(Clone)]
pub struct FileDescriptor {
    pub flags: OpenFlags,
    pub file: FileClass,
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
    pub fn file(&self) -> Result<Arc<OSFile>, SysErrNo> {
        self.file.file()
    }
    pub fn socket(&self)->Result<Arc<Socket>,SysErrNo>{
        self.file.file()
    }
    pub fn abs(&self) -> Result<Arc<dyn File>, SysErrNo> {
        self.file.abs()
    }
    pub fn any(&self) -> Arc<dyn File> {
        self.file.any()
    }

    pub fn unset_cloexec(&mut self) {
        self.flags &= !OpenFlags::O_CLOEXEC;
    }
    pub fn set_cloexec(&mut self) {
        self.flags |= OpenFlags::O_CLOEXEC;
    }
    pub fn cloexec(&self) -> bool {
        self.flags.contains(OpenFlags::O_CLOEXEC)
    }
    pub fn non_block(&self) -> bool {
        self.flags.contains(OpenFlags::O_NONBLOCK)
    }
    pub fn unset_nonblock(&mut self) {
        self.flags &= !OpenFlags::O_NONBLOCK;
    }
    pub fn set_nonblock(&mut self) {
        self.flags |= OpenFlags::O_NONBLOCK;
    }
}

struct FdTableInner {
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
    fn new(fd_table: FdTableInner) -> Self {
        Self {
            inner: SyncUnsafeCell::new(fd_table),
        }
    }
    pub fn new_with_stdio() -> Self {
        FdTable::new(FdTableInner::new(
            128,
            256,
            vec![
                Some(FileDescriptor::default(FileClass::Abs(Arc::new(Stdin)))),
                Some(FileDescriptor::default(FileClass::Abs(Arc::new(Stdout)))),
                Some(FileDescriptor::default(FileClass::Abs(Arc::new(Stdout)))),
            ],
        ))
    }
    pub fn from_another(another: &Arc<FdTable>) -> Self {
        let other = another.get_ref();
        Self {
            inner: SyncUnsafeCell::new(FdTableInner {
                soft_limit: other.soft_limit,
                hard_limit: other.hard_limit,
                files: other.files.clone(),
            }),
        }
    }
    pub fn clear(&self) {
        self.get_mut().files.clear();
    }
    pub fn alloc_fd(&self) -> SyscallRet {
        let fd_table = &mut self.get_mut().files;
        if let Some(fd) = (0..fd_table.len()).find(|fd| fd_table[*fd].is_none()) {
            return Ok(fd);
        }
        if fd_table.len() + 1 > self.get_soft_limit() {
            return Err(SysErrNo::EMFILE);
        }
        fd_table.push(None);
        Ok(fd_table.len() - 1)
    }
    pub fn alloc_fd_larger_than(&self, arg: usize) -> SyscallRet {
        let fd_table = &mut self.get_mut().files;
        if arg > self.get_soft_limit() {
            return Err(SysErrNo::EMFILE);
        }
        if fd_table.len() + 1 > self.get_soft_limit() {
            return Err(SysErrNo::EMFILE);
        }
        if fd_table.len() < arg {
            fd_table.resize(arg, None);
        }
        if let Some(fd) = (arg..fd_table.len()).find(|fd| fd_table[*fd].is_none()) {
            Ok(fd)
        } else {
            fd_table.push(None);
            Ok(fd_table.len() - 1)
        }
    }
    pub fn close_on_exec(&self) {
        let fd_table = &mut self.get_mut().files;
        for idx in 0..fd_table.len() {
            if fd_table[idx].is_some() && fd_table[idx].as_ref().unwrap().cloexec() {
                fd_table[idx].take();
            }
        }
    }
    pub fn len(&self) -> usize {
        self.get_ref().files.len()
    }

    pub fn resize(&self, size: usize) -> GeneralRet {
        if size > self.get_soft_limit() {
            return Err(SysErrNo::EMFILE);
        }
        self.get_mut().files.resize(size, None);
        Ok(())
    }

    pub fn try_get(&self, fd: usize) -> Option<FileDescriptor> {
        self.get_mut().files[fd].clone()
    }

    pub fn get(&self, fd: usize) -> FileDescriptor {
        self.get_mut().files[fd].clone().unwrap()
    }

    pub fn set_cloexec(&self, fd: usize) {
        self.get_mut().files[fd].as_mut().unwrap().set_cloexec();
    }

    pub fn unset_cloexec(&self, fd: usize) {
        self.get_mut().files[fd].as_mut().unwrap().unset_cloexec();
    }

    pub fn set_nonblock(&self, fd: usize) {
        self.get_mut().files[fd].as_mut().unwrap().set_nonblock();
    }

    pub fn unset_nonblock(&self, fd: usize) {
        self.get_mut().files[fd].as_mut().unwrap().unset_nonblock();
    }

    pub fn get_hard_limit(&self) -> usize {
        self.get_ref().hard_limit
    }

    pub fn get_soft_limit(&self) -> usize {
        self.get_ref().soft_limit
    }

    pub fn set_limit(&self, soft_limit: usize, hard_limit: usize) {
        let inner = self.get_mut();
        inner.soft_limit = soft_limit;
        inner.hard_limit = hard_limit;
    }

    pub fn set(&self, fd: usize, file: FileDescriptor) {
        self.get_mut().files[fd] = Some(file);
    }

    pub fn set_flags(&self, fd: usize, file: FileDescriptor) {
        self.get_mut().files[fd] = Some(file);
    }

    pub fn take(&self, fd: usize) -> Option<FileDescriptor> {
        self.get_mut().files[fd].take()
    }

    fn get_mut(&self) -> &mut FdTableInner {
        self.inner.get_unchecked_mut()
    }

    fn get_ref(&self) -> &FdTableInner {
        self.inner.get_unchecked_ref()
    }
}
