use crate::{
    fs::{files::OSFile, Pipe, Socket},
    mm::UserBuffer,
    net::{Shutdown, SocketOps},
    syscall::Syscall,
    utils::{SysErrNo, SysResult, SyscallRet},
};
use alloc::{sync::Arc, vec, vec::Vec};

use log::debug;
use spin::rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::{DetachedMountFd, File, FileClass, FsContextFd, OpenFlags, Stdin, Stdout};
pub struct FdTable {
    inner: RwLock<FdTableInner>,
}

#[derive(Clone)]
pub struct FileDescriptor {
    flags: OpenFlags,
    file: FileClass,
}

impl FileDescriptor {
    /// 创建一个带指定 open flags 和文件对象分类的文件描述符。
    pub fn new(flags: OpenFlags, file: FileClass) -> Self {
        Self { flags, file }
    }

    /// 创建一个不带额外 flags 的文件描述符。
    pub fn default(file: FileClass) -> Self {
        Self {
            flags: OpenFlags::empty(),
            file,
        }
    }

    /// 返回文件描述符内部保存的完整 flags 位图。
    pub fn flags(&self) -> u32 {
        self.flags.bits()
    }

    /// 返回 Linux `fcntl(F_GETFL)` 可见的访问模式和文件状态 flags。
    ///
    /// 创建时 flags 和 fd descriptor flags（例如 `O_CREAT`、`O_EXCL`、
    /// `O_TRUNC`、`O_CLOEXEC`）不会通过 `F_GETFL` 暴露。
    pub fn getfl_flags(&self) -> u32 {
        let visible = OpenFlags::O_ACCMODE
            | OpenFlags::O_APPEND
            | OpenFlags::O_NONBLOCK
            | OpenFlags::O_DSYNC
            | OpenFlags::O_SYNC
            | OpenFlags::O_RSYNC
            | OpenFlags::O_ASYNC
            | OpenFlags::O_DIRECT
            | OpenFlags::O_LARGEFILE
            | OpenFlags::O_NOATIME
            | OpenFlags::O_PATH;
        (self.flags & visible).bits()
    }

    /// 判断该 fd 是否是 `O_PATH` 路径句柄。
    pub fn is_path_only(&self) -> bool {
        self.flags.contains(OpenFlags::O_PATH)
    }

    /// 以普通 `OSFile` 类型取出文件对象。
    pub fn file(&self) -> Result<Arc<OSFile>, SysErrNo> {
        self.file.file()
    }

    /// 以 pipe 类型取出文件对象。
    pub fn pipe(&self) -> SysResult<Arc<Pipe>> {
        self.file.pipe()
    }

    /// 以通用 `File` trait object 类型取出文件对象。
    pub fn abs(&self) -> Result<Arc<dyn File>, SysErrNo> {
        self.file.abs()
    }

    /// 以 fsopen/fsconfig 使用的 filesystem context fd 类型取出对象。
    pub fn fs_context(&self) -> Result<Arc<FsContextFd>, SysErrNo> {
        self.file.fs_context()
    }

    /// 以 open_tree/fsmount 使用的 detached mount fd 类型取出对象。
    pub fn detached_mount(&self) -> Result<Arc<DetachedMountFd>, SysErrNo> {
        self.file.detached_mount()
    }

    /// 以 socket 类型取出文件对象。
    pub fn socket(&self) -> Result<Arc<Socket>, SysErrNo> {
        self.file.socket()
    }

    /// 返回该描述符指向的任意文件对象。
    pub fn any(&self) -> Arc<dyn File> {
        self.file.any()
    }

    /// 若该描述符是 socket 且没有其它强引用，则主动 shutdown socket。
    fn close_socket_if_last_ref(&self) {
        if let FileClass::Socket(socket) = &self.file {
            if Arc::strong_count(socket) == 1 {
                let _ = socket.0.shutdown(Shutdown::Both);
            }
        }
    }

    /// 进程 fd 表整体清理时主动 shutdown socket。
    fn shutdown_socket(&self) {
        if let FileClass::Socket(socket) = &self.file {
            let _ = socket.0.shutdown(Shutdown::Both);
        }
    }

    /// 判断同一 fd 表中是否还有别的 fd 指向同一个 socket 对象。
    fn has_fd_alias(&self, files: &[Option<FileDescriptor>]) -> bool {
        let FileClass::Socket(socket) = &self.file else {
            return false;
        };
        files.iter().flatten().any(
            |desc| matches!(&desc.file, FileClass::Socket(other) if Arc::ptr_eq(socket, other)),
        )
    }

    /// 清除该描述符的 close-on-exec 标志。
    pub fn unset_cloexec(&mut self) {
        self.flags.remove(OpenFlags::O_CLOEXEC);
    }

    /// 设置该描述符的 close-on-exec 标志。
    pub fn set_cloexec(&mut self) {
        self.flags.insert(OpenFlags::O_CLOEXEC);
    }

    /// 判断该描述符是否带 close-on-exec 标志。
    pub fn cloexec(&self) -> bool {
        self.flags.contains(OpenFlags::O_CLOEXEC)
    }

    /// 判断该描述符是否带非阻塞 I/O 标志。
    pub fn non_block(&self) -> bool {
        self.flags.contains(OpenFlags::O_NONBLOCK)
    }

    /// 清除该描述符的非阻塞 I/O 标志。
    pub fn unset_nonblock(&mut self) {
        self.flags.remove(OpenFlags::O_NONBLOCK);
    }

    /// 设置该描述符的非阻塞 I/O 标志。
    pub fn set_nonblock(&mut self) {
        self.flags.insert(OpenFlags::O_NONBLOCK);
    }

    /// 按 `fcntl(F_SETFL)` 语义更新可修改的文件状态 flags。
    ///
    /// 访问模式和创建 flags 不会被 `F_SETFL` 修改。
    fn set_status_flags(&mut self, flags: OpenFlags) {
        let mutable = OpenFlags::O_APPEND
            | OpenFlags::O_NONBLOCK
            | OpenFlags::O_ASYNC
            | OpenFlags::O_DIRECT
            | OpenFlags::O_NOATIME;
        self.flags.remove(mutable);
        self.flags.insert(flags & mutable);
    }
}

pub struct FdTableInner {
    soft_limit: usize,
    hard_limit: usize,
    files: Vec<Option<FileDescriptor>>,
}

impl FdTableInner {
    /// 创建一个空 fd 表内部状态，使用默认 soft/hard limit。
    pub fn empty() -> Self {
        Self {
            soft_limit: 128,
            hard_limit: 256,
            files: Vec::new(),
        }
    }

    /// 用指定 limits 和 fd 槽内容创建 fd 表内部状态。
    pub fn new(soft_limit: usize, hard_limit: usize, files: Vec<Option<FileDescriptor>>) -> Self {
        Self {
            soft_limit,
            hard_limit,
            files,
        }
    }
}

impl FdTable {
    /// 创建新的 fd table。
    fn new(fd_table: FdTableInner) -> Self {
        Self {
            inner: RwLock::new(fd_table),
        }
    }

    /// 创建带标准输入、标准输出和标准错误的 fd table。
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

    /// 克隆一份 fd table，用于 fork/clone 时复制文件描述符表。
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

    /// 清空 fd table，并主动关闭表中 socket 资源。
    pub fn clear(&self) {
        let files = {
            let mut inner = self.get_mut();
            core::mem::take(&mut inner.files)
        };
        for desc in files.into_iter().flatten() {
            desc.shutdown_socket();
        }
    }

    /// 分配当前 soft limit 内最小的可用 fd 槽。
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

    /// 分配一个不小于 `arg` 的可用 fd 槽，用于 `F_DUPFD` 等接口。
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

    /// 执行 exec 时关闭所有带 `O_CLOEXEC` 的 fd。
    pub fn close_on_exec(&self) {
        let fds = {
            let inner = self.get_ref();
            inner
                .files
                .iter()
                .enumerate()
                .filter_map(|(fd, desc)| {
                    desc.as_ref()
                        .filter(|desc| desc.flags.contains(OpenFlags::O_CLOEXEC))
                        .map(|_| fd)
                })
                .collect::<Vec<_>>()
        };
        for fd in fds {
            self.close(fd);
        }
    }

    /// 返回 fd 槽数组当前长度。
    pub fn len(&self) -> usize {
        self.get_ref().files.len()
    }

    /// 判断 fd 槽数组是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 调整 fd 槽数组大小，不能超过 soft limit。
    pub fn resize(&self, size: usize) -> SysResult {
        let mut inner = self.get_mut();
        let soft_limit = inner.soft_limit;
        let fd_table = &mut inner.files;
        if size > soft_limit {
            return Err(SysErrNo::EMFILE);
        }
        fd_table.resize(size, None);
        Ok(())
    }

    /// 尝试获取文件描述符副本；fd 不存在或槽为空时返回 `None`。
    pub fn try_get(&self, fd: usize) -> Option<FileDescriptor> {
        self.get_ref()
            .files
            .get(fd) // 安全检查边界
            .and_then(|x| x.as_ref()) // 检查是否为 Some
            .cloned()
    }

    /// 获取文件描述符副本；fd 不存在或槽为空时返回 `EBADF`。
    pub fn get(&self, fd: usize) -> Result<FileDescriptor, SysErrNo> {
        self.try_get(fd).ok_or(SysErrNo::EBADF)
    }

    /// 读取指定 fd 的 close-on-exec 状态。
    pub fn get_cloexec(&self, fd: usize) -> Result<bool, SysErrNo> {
        let fd_inner = self.get_ref();
        let desc = fd_inner
            .files
            .get(fd)
            .and_then(|x| x.as_ref())
            .ok_or(SysErrNo::EBADF)?;
        Ok(desc.flags.contains(OpenFlags::O_CLOEXEC))
    }

    /// 为指定 fd 设置 close-on-exec 标志。
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

    /// 清除指定 fd 的 close-on-exec 标志。
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

    /// 为指定 fd 设置非阻塞 I/O 标志。
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

    /// 清除指定 fd 的非阻塞 I/O 标志。
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

    /// 按 `fcntl(F_SETFL)` 语义更新指定 fd 的文件状态 flags。
    pub fn set_status_flags(&self, fd: usize, flags: OpenFlags) -> SyscallRet {
        let mut inner = self.get_mut();
        let desc = inner
            .files
            .get_mut(fd)
            .and_then(|x| x.as_mut())
            .ok_or(SysErrNo::EBADF)?;
        desc.set_status_flags(flags);
        Ok(0)
    }

    /// 返回 fd table 的 hard limit。
    pub fn get_hard_limit(&self) -> usize {
        self.get_ref().hard_limit
    }

    /// 返回 fd table 的 soft limit。
    pub fn get_soft_limit(&self) -> usize {
        self.get_ref().soft_limit
    }

    /// 设置 fd table 的 soft/hard limit。
    pub fn set_limit(&self, soft_limit: usize, hard_limit: usize) {
        let mut inner = self.get_mut();
        inner.soft_limit = soft_limit;
        inner.hard_limit = hard_limit;
    }

    /// 将指定 fd 槽设置为给定文件描述符。
    ///
    /// 若原槽已有描述符且没有其它 fd 引用同一 socket，会主动关闭旧 socket。
    pub fn set(&self, fd: usize, file: FileDescriptor) -> Result<(), SysErrNo> {
        let old = {
            let mut inner = self.get_mut();
            if fd >= inner.soft_limit {
                return Err(SysErrNo::EMFILE);
            }
            if fd >= inner.files.len() {
                inner.files.resize(fd + 1, None);
            }
            let old = inner.files[fd].replace(file);
            old.map(|desc| {
                let should_close = !desc.has_fd_alias(&inner.files);
                (desc, should_close)
            })
        };
        if let Some((desc, true)) = old {
            desc.close_socket_if_last_ref();
        }
        Ok(())
    }

    /// 兼容旧调用点的 fd 设置接口；忽略 `set()` 的错误返回。
    pub fn set_flags(&self, fd: usize, file: FileDescriptor) {
        let _ = self.set(fd, file);
    }

    /// 从 fd 表中取出并清空指定 fd 槽，不执行 socket shutdown。
    pub fn take(&self, fd: usize) -> Option<FileDescriptor> {
        let mut inner = self.get_mut();
        inner.files.get_mut(fd).and_then(|slot| slot.take())
    }

    /// 关闭指定 fd 并返回原文件描述符。
    ///
    /// 若关闭的是 fd 表中最后一个指向该 socket 的描述符，会主动 shutdown socket。
    pub fn close(&self, fd: usize) -> Option<FileDescriptor> {
        let closed = {
            let mut inner = self.get_mut();
            let desc = inner.files.get_mut(fd).and_then(|slot| slot.take())?;
            let should_close = !desc.has_fd_alias(&inner.files);
            (desc, should_close)
        };
        if closed.1 {
            closed.0.close_socket_if_last_ref();
        }
        Some(closed.0)
    }

    /// 获取 fd table 内部写锁。
    fn get_mut(&self) -> RwLockWriteGuard<'_, FdTableInner> {
        self.inner.write()
    }

    /// 获取 fd table 内部读锁。
    fn get_ref(&self) -> RwLockReadGuard<'_, FdTableInner> {
        self.inner.read()
    }

    /// 在持有 fd table 写锁时对指定文件描述符执行闭包。
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

    /// 尝试获取指定 fd 对应的通用文件对象。
    ///
    /// 调用者必须保证 `fd` 在当前 fd 槽数组范围内。
    pub fn try_get_file(&self, fd: usize) -> Option<Arc<dyn File>> {
        self.get_mut().files[fd].as_mut().map(|f| f.any())
    }
}
