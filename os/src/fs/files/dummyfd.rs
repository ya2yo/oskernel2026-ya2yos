//! 这是一个临时文件，这里实现的是虚假的文件描述符供那些没有真正实现的文件描述符使用

use alloc::sync::Arc;

use super::super::{File, Kstat};
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SyscallRet};

pub struct DummyFd;

impl DummyFd {
    pub fn new() -> Arc<Self> {
        Arc::new(DummyFd {})
    }
}

impl File for DummyFd {
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        true
    }

    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn fstat(&self) -> Kstat {
        Kstat::default()
    }

    fn poll(&self, _events: PollEvents) -> PollEvents {
        PollEvents::empty()
    }
}
