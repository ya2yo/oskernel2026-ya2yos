//! 虚拟文件描述符的最小占位实现。
//!
//! 当某个系统调用已经能够创建并返回一个文件描述符，但对应的内核对象
//! 尚未具备真实的读写语义时，可暂时使用 [`DummyFd`] 作为其底层文件。
//! 该对象只负责满足文件表和 `File` trait 的类型要求，不伪造任何有效数据：
//! 读写操作统一返回 `EINVAL`，轮询也不会报告就绪事件。

use alloc::sync::Arc;

use super::super::{File, Kstat};
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SyscallRet};

/// 不提供实际数据的占位文件对象。
///
/// 该类型用于暂未实现完整语义的文件描述符，所有实例均无内部状态。
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
