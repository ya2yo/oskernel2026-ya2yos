//! eventfd 文件对象：内核线程/进程间事件通知。
//!
//! 参考 https://man7.org/linux/man-pages/man2/eventfd.2.html
//!
//! 系统调用入口在 [`crate::syscall::fs::event::sys_eventfd2`]。
//!
//! [`EventFd`] 包含一个由内核维护的 64 位无符号计数器。
//! * `read` 读取计数器值，读取后计数器归零（或信号量模式下减 1）。
//! * `write` 将写入值累加到计数器上。
//! * 当计数器 = 0 时 `read` 阻塞；当计数器 = u64::MAX-1 时 `write` 阻塞。

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{
    fs::{vfs::File, Kstat},
    mm::UserBuffer,
    syscall::PollEvents,
    task::{block_on, poll_io},
    utils::{PollSet, SysErrNo, SysResult, SyscallRet},
};

/// eventfd 文件对象。
///
/// 支持两种模式：
/// * **普通模式** (`semaphore = false`)：`read` 返回并清零计数器。
/// * **信号量模式** (`semaphore = true`)：`read` 返回 1 并将计数器减 1。
pub struct EventFd {
    /// 64 位无符号整数计数器
    counter: AtomicU64,
    /// 是否为信号量语义（EFD_SEMAPHORE）
    semaphore: bool,
    /// 是否非阻塞
    non_blocking: AtomicBool,
    /// 读端唤醒器集合：写入方向计数器累加后唤醒等待的读者
    poll_rx: PollSet,
    /// 写端唤醒器集合：读取方向腾出空间后唤醒等待的写者
    poll_tx: PollSet,
}

impl EventFd {
    /// 创建新的 eventfd 实例。
    ///
    /// # 参数
    /// * `initval` — 计数器初始值
    /// * `semaphore` — 是否启用信号量语义
    pub fn new(initval: u64, semaphore: bool) -> Arc<Self> {
        Arc::new(Self {
            counter: AtomicU64::new(initval),
            semaphore,
            non_blocking: AtomicBool::new(false),
            poll_rx: PollSet::new(),
            poll_tx: PollSet::new(),
        })
    }
}

impl File for EventFd {
    /// 读取计数器值。
    ///
    /// * 普通模式：读取后计数器归零。
    /// * 信号量模式：读取固定返回 1，计数器减 1。
    /// * 计数器为 0 且阻塞模式：挂起当前任务直到写入发生。
    /// * 返回读取的字节数（固定为 `size_of::<u64>()`）。
    fn read(&self, mut dstbuf: UserBuffer) -> SyscallRet {
        if dstbuf.len() < size_of::<u64>() {
            return Err(SysErrNo::EINVAL);
        }

        block_on(poll_io(self, PollEvents::IN, self.nonblocking(), || {
            let result = self
                .counter
                .fetch_update(Ordering::Release, Ordering::Acquire, |count| {
                    if count > 0 {
                        let dec = if self.semaphore { 1 } else { count };
                        Some(count - dec)
                    } else {
                        None
                    }
                });
            match result {
                Ok(count) => {
                    dstbuf.write(&count.to_ne_bytes());
                    self.poll_tx.wake();
                    Ok(size_of::<u64>())
                }
                Err(_) => Err(SysErrNo::EAGAIN),
            }
        }))
    }

    /// 将写入值累加到计数器。
    ///
    /// * 读取 `srcbuf` 中的 `u64` 值并加到计数器上。
    /// * 值 = `u64::MAX` 无效，返回 `EINVAL`。
    /// * 溢出（`u64::MAX - count < val`）时阻塞直到有读取腾出空间。
    /// * 返回写入的字节数（固定为 `size_of::<u64>()`）。
    fn write(&self, srcbuf: UserBuffer) -> SyscallRet {
        let len = size_of::<u64>();
        if srcbuf.len() < len {
            return Err(SysErrNo::EINVAL);
        }
        let mut val = [0u8; 8];
        srcbuf.read_to(&mut val);
        let val = u64::from_ne_bytes(val);
        if val == u64::MAX {
            return Err(SysErrNo::EINVAL);
        }

        block_on(poll_io(self, PollEvents::OUT, self.nonblocking(), || {
            let result = self
                .counter
                .fetch_update(Ordering::Release, Ordering::Acquire, |count| {
                    if u64::MAX - count > val {
                        Some(count + val)
                    } else {
                        None
                    }
                });
            match result {
                Ok(_) => {
                    self.poll_rx.wake();
                    Ok(len)
                }
                Err(_) => Err(SysErrNo::EAGAIN),
            }
        }))
    }

    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        true
    }

    fn fstat(&self) -> Kstat {
        Kstat::default()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> SysResult {
        self.non_blocking.store(nonblocking, Ordering::Relaxed);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        let count = self.counter.load(Ordering::Acquire);
        if events.contains(PollEvents::IN) {
            revents.set(PollEvents::IN, count > 0);
        }
        if events.contains(PollEvents::OUT) {
            revents.set(PollEvents::OUT, count < u64::MAX - 1);
        }
        revents
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: PollEvents) {
        if events.contains(PollEvents::IN) {
            self.poll_rx.register(context.waker());
        }
        if events.contains(PollEvents::OUT) {
            self.poll_tx.register(context.waker());
        }
    }
}
