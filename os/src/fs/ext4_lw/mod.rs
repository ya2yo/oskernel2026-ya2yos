//! lwext4-backed EXT4 文件系统适配层。
//!
//! 该模块把第三方 `lwext4_rust` wrapper 暴露的路径式文件系统接口适配成
//! Ya2yOS 内部的 VFS trait：
//!
//! - [`inode`]：将 `Ext4File` 包装成 VFS [`Inode`](crate::fs::Inode)。
//! - [`sb`]：维护全局超级块、挂载点状态和块设备读写回调。
//!
//! 上层 syscall/VFS 只依赖 `superblock_*` helper 和 `Inode` trait，不直接接触
//! lwext4 的 C 风格接口。

mod inode;
mod sb;

use core::{future::poll_fn, task::Poll};

use crate::utils::PollSet;

/// lwext4 shares one mounted block cache and does not provide SMP-safe internal
/// locking. Keep every call into its path/file API serialized until the wrapper
/// gains per-superblock concurrency support.
///
/// The critical sections include block-device I/O and may last milliseconds.
/// Spinning every competing Cargo task for that interval starves the lock owner
/// under QEMU SMP.  Contended task-context callers therefore sleep on
/// `waiters`; boot-time callers without a current task retain the spin fallback.
pub(super) struct Ext4OpLock {
    inner: spin::Mutex<()>,
    waiters: PollSet,
}

pub(super) struct Ext4OpGuard<'a> {
    lock: &'a Ext4OpLock,
    guard: Option<spin::MutexGuard<'a, ()>>,
    wait_ticks: usize,
    acquired_at: usize,
}

/// Lightweight lock classes identify the most contended lwext4 entry points
/// without changing its global serialization model.
#[derive(Clone, Copy)]
pub(super) enum Ext4LockClass {
    Read,
    Find,
    Fstat,
    Write,
    Rename,
}

pub(super) struct Ext4ProfiledOpGuard<'a> {
    guard: Ext4OpGuard<'a>,
    class: Ext4LockClass,
}

impl Ext4OpLock {
    pub const fn new() -> Self {
        Self {
            inner: spin::Mutex::new(()),
            waiters: PollSet::new(),
        }
    }

    pub fn lock(&self) -> Ext4OpGuard<'_> {
        let wait_start = crate::arch::time::get_ticks();
        let guard = match self.inner.try_lock() {
            Some(guard) => guard,
            None if crate::task::current_task().is_none() => self.inner.lock(),
            None => crate::task::block_on(poll_fn(|cx| {
                if let Some(guard) = self.inner.try_lock() {
                    return Poll::Ready(guard);
                }

                // Register before the second attempt so an unlock cannot be
                // lost between observing contention and blocking this task.
                self.waiters.register(cx.waker());
                match self.inner.try_lock() {
                    Some(guard) => {
                        // A release may have happened between the first probe
                        // and registration. This task no longer waits, so do
                        // not leave a stale wakeup ahead of another waiter.
                        self.waiters.unregister(cx.waker());
                        Poll::Ready(guard)
                    }
                    None => Poll::Pending,
                }
            })),
        };
        let acquired_at = crate::arch::time::get_ticks();
        Ext4OpGuard {
            lock: self,
            guard: Some(guard),
            wait_ticks: acquired_at.saturating_sub(wait_start),
            acquired_at,
        }
    }

    pub fn lock_for_read(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Read)
    }

    pub fn lock_for_find(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Find)
    }

    pub fn lock_for_fstat(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Fstat)
    }

    pub fn lock_for_write(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Write)
    }

    pub fn lock_for_rename(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Rename)
    }

    fn lock_profiled(&self, class: Ext4LockClass) -> Ext4ProfiledOpGuard<'_> {
        Ext4ProfiledOpGuard {
            guard: self.lock(),
            class,
        }
    }
}

impl Drop for Ext4OpGuard<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "perf")]
        let released_at = crate::arch::time::get_ticks();
        // Drop the primitive mutex before waking waiters.  A woken task can
        // acquire it immediately instead of bouncing through another sleep.
        self.guard.take();
        // Hand off to one waiter. Waking every blocked Cargo task here makes
        // all of them race for the same non-SMP-safe lwext4 instance.
        self.lock.waiters.wake_one();
        #[cfg(feature = "perf")]
        crate::utils::perf::record_ext4_lock(
            self.wait_ticks,
            released_at.saturating_sub(self.acquired_at),
        );
    }
}

impl Drop for Ext4ProfiledOpGuard<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "perf")]
        {
            let released_at = crate::arch::time::get_ticks();
            let hold_ticks = released_at.saturating_sub(self.guard.acquired_at);
            match self.class {
                Ext4LockClass::Read => {
                    crate::utils::perf::record_ext4_read_lock(self.guard.wait_ticks, hold_ticks)
                }
                Ext4LockClass::Find => {
                    crate::utils::perf::record_ext4_find_lock(self.guard.wait_ticks, hold_ticks)
                }
                Ext4LockClass::Fstat => {
                    crate::utils::perf::record_ext4_fstat_lock(self.guard.wait_ticks, hold_ticks)
                }
                Ext4LockClass::Write => {
                    crate::utils::perf::record_ext4_write_lock(self.guard.wait_ticks, hold_ticks)
                }
                Ext4LockClass::Rename => {
                    crate::utils::perf::record_ext4_rename_lock(self.guard.wait_ticks, hold_ticks)
                }
            }
        }
    }
}

pub(super) static EXT4_OP_LOCK: Ext4OpLock = Ext4OpLock::new();

pub use inode::*;
pub use sb::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
