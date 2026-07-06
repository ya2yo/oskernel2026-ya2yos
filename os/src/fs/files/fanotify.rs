//! fanotify 实例文件对象。
//!
//! 当前只实现 `fanotify_init(2)` 创建出的 fd 载体和基础 fd 行为。实际 mark
//! 管理、VFS 事件投递和权限事件响应需要在 `fanotify_mark(2)` 及文件系统路径中
//! 继续接入。

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{
    fs::{vfs::File, Kstat},
    mm::UserBuffer,
    syscall::PollEvents,
    utils::{SysErrNo, SysResult, SyscallRet},
};

/// 一个 fanotify notification group。
pub struct FanotifyFd {
    init_flags: u32,
    event_f_flags: u32,
    nonblocking: AtomicBool,
}

impl FanotifyFd {
    /// 创建 fanotify 实例。
    pub fn new(init_flags: u32, event_f_flags: u32, nonblocking: bool) -> Arc<Self> {
        Arc::new(Self {
            init_flags,
            event_f_flags,
            nonblocking: AtomicBool::new(nonblocking),
        })
    }

    /// 创建时传给 `fanotify_init()` 的 fanotify flags。
    pub fn init_flags(&self) -> u32 {
        self.init_flags
    }

    /// 后续事件 fd 使用的 open flags。
    pub fn event_f_flags(&self) -> u32 {
        self.event_f_flags
    }
}

impl File for FanotifyFd {
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        false
    }

    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EAGAIN)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn fstat(&self) -> Kstat {
        Kstat::default()
    }

    fn nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, nonblocking: bool) -> SysResult {
        self.nonblocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    fn poll(&self, _events: PollEvents) -> PollEvents {
        PollEvents::empty()
    }
}
