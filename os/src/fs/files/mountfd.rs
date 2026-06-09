use alloc::{borrow::Cow, string::String, sync::Arc, vec::Vec};
use spin::Mutex;

use crate::{
    fs::{File, Kstat, StMode},
    mm::UserBuffer,
    syscall::PollEvents,
    utils::{SysErrNo, SyscallRet},
};

#[derive(Clone)]
pub enum FsConfigValue {
    Flag,
    String(String),
    Binary(Vec<u8>),
    Path { path: String, dirfd: i32 },
    Fd(i32),
}

#[derive(Clone)]
pub struct FsConfigOption {
    pub key: String,
    pub value: FsConfigValue,
}

pub struct FsContext {
    pub fsname: String,
    pub source: Option<String>,
    pub options: Vec<FsConfigOption>,
    pub created: bool,
    pub exclusive: bool,
    pub reconfigure: bool,
}

pub struct FsContextFd {
    inner: Mutex<FsContext>,
}

impl FsContextFd {
    pub fn new(fsname: String) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(FsContext {
                fsname,
                source: None,
                options: Vec::new(),
                created: false,
                exclusive: false,
                reconfigure: false,
            }),
        })
    }

    pub fn picked(source: String) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(FsContext {
                fsname: String::from(""),
                source: Some(source),
                options: Vec::new(),
                created: true,
                exclusive: false,
                reconfigure: true,
            }),
        })
    }

    pub fn with_inner<T>(&self, f: impl FnOnce(&mut FsContext) -> T) -> T {
        let mut inner = self.inner.lock();
        f(&mut inner)
    }

    fn file_stat() -> Kstat {
        Kstat {
            st_mode: StMode::FREG.bits() | 0o600,
            st_nlink: 1,
            st_blksize: 512,
            ..Kstat::default()
        }
    }
}

impl File for FsContextFd {
    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn fstat(&self) -> Kstat {
        Self::file_stat()
    }

    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed("anon_inode:[fscontext]")
    }

    fn poll(&self, _events: PollEvents) -> PollEvents {
        PollEvents::empty()
    }
}

pub struct DetachedMountFd {
    pub fsname: String,
    pub source: Option<String>,
    pub flags: u32,
    pub attr_flags: u32,
}

impl DetachedMountFd {
    pub fn new(fsname: String, source: Option<String>, flags: u32, attr_flags: u32) -> Arc<Self> {
        Arc::new(Self {
            fsname,
            source,
            flags,
            attr_flags,
        })
    }
}

impl File for DetachedMountFd {
    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn fstat(&self) -> Kstat {
        FsContextFd::file_stat()
    }

    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed("anon_inode:[fsmount]")
    }

    fn poll(&self, _events: PollEvents) -> PollEvents {
        PollEvents::empty()
    }
}
