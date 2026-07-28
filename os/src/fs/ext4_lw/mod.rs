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

/// A task-aware mutex for filesystem operations that can block on I/O.
///
/// Contended task-context callers sleep instead of spinning, while boot-time
/// callers without a current task retain the spin fallback.  It is used both
/// for the mount-wide lwext4 gate and for per-inode state that must stay stable
/// while a cache-only write bypasses that global gate.
pub(super) struct TaskMutex {
    inner: spin::Mutex<()>,
    waiters: PollSet,
}

pub(super) struct TaskMutexGuard<'a> {
    lock: &'a TaskMutex,
    guard: Option<spin::MutexGuard<'a, ()>>,
    wait_ticks: usize,
    acquired_at: usize,
}

/// lwext4 shares one mounted block cache and does not provide SMP-safe internal
/// locking. Keep every call into its path/file API serialized until the wrapper
/// gains per-superblock concurrency support.
pub(super) struct Ext4OpLock {
    inner: TaskMutex,
}

pub(super) struct Ext4OpGuard<'a> {
    guard: Option<TaskMutexGuard<'a>>,
    wait_ticks: usize,
    acquired_at: usize,
}

/// Lightweight lock classes identify the most contended lwext4 entry points
/// without changing its global serialization model.
#[derive(Clone, Copy)]
pub(super) enum Ext4LockClass {
    ReadOpen,
    ReadData,
    Find,
    Fstat,
    Write,
    Rename,
    Close,
    ReadAll,
    ReadDir,
    PathResolve,
    Metadata,
    Namespace,
    Sync,
    Seek,
}

pub(super) struct Ext4ProfiledOpGuard<'a> {
    guard: Ext4OpGuard<'a>,
    class: Ext4LockClass,
}

impl TaskMutex {
    pub const fn new() -> Self {
        Self {
            inner: spin::Mutex::new(()),
            waiters: PollSet::new(),
        }
    }

    pub fn lock(&self) -> TaskMutexGuard<'_> {
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
        TaskMutexGuard {
            lock: self,
            guard: Some(guard),
            wait_ticks: acquired_at.saturating_sub(wait_start),
            acquired_at,
        }
    }
}

impl Drop for TaskMutexGuard<'_> {
    fn drop(&mut self) {
        // Drop the primitive mutex before waking a single sleeper.  Waking all
        // waiters would make unrelated compiler tasks contend on the same
        // filesystem state again immediately.
        self.guard.take();
        self.lock.waiters.wake_one();
    }
}

impl Ext4OpLock {
    pub const fn new() -> Self {
        Self {
            inner: TaskMutex::new(),
        }
    }

    pub fn lock(&self) -> Ext4OpGuard<'_> {
        let guard = self.inner.lock();
        Ext4OpGuard {
            wait_ticks: guard.wait_ticks,
            acquired_at: guard.acquired_at,
            guard: Some(guard),
        }
    }

    pub fn lock_for_read_open(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::ReadOpen)
    }

    pub fn lock_for_read_data(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::ReadData)
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

    pub fn lock_for_close(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Close)
    }

    pub fn lock_for_read_all(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::ReadAll)
    }

    pub fn lock_for_read_dir(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::ReadDir)
    }

    pub fn lock_for_path_resolve(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::PathResolve)
    }

    pub fn lock_for_metadata(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Metadata)
    }

    pub fn lock_for_namespace(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Namespace)
    }

    pub fn lock_for_sync(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Sync)
    }

    pub fn lock_for_seek(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Seek)
    }

    fn lock_profiled(&self, class: Ext4LockClass) -> Ext4ProfiledOpGuard<'_> {
        Ext4ProfiledOpGuard {
            guard: self.lock(),
            class,
        }
    }
}

impl Ext4OpGuard<'_> {
    #[cfg(feature = "perf")]
    fn release(&mut self) -> Option<(usize, usize)> {
        let released_at = crate::arch::time::get_ticks();
        self.guard.take().map(|_| {
            (
                self.wait_ticks,
                released_at.saturating_sub(self.acquired_at),
            )
        })
    }
}

impl Drop for Ext4OpGuard<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "perf")]
        if let Some((wait_ticks, hold_ticks)) = self.release() {
            // The primitive guard is released before the relaxed atomics so a
            // newly woken task never queues behind profiling bookkeeping.
            crate::utils::perf::record_ext4_lock(wait_ticks, hold_ticks);
        }
        #[cfg(not(feature = "perf"))]
        self.guard.take();
    }
}

impl Drop for Ext4ProfiledOpGuard<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "perf")]
        {
            if let Some((wait_ticks, hold_ticks)) = self.guard.release() {
                match self.class {
                    Ext4LockClass::ReadOpen => {
                        crate::utils::perf::record_ext4_read_lock(wait_ticks, hold_ticks);
                        crate::utils::perf::record_ext4_read_open_lock(wait_ticks, hold_ticks);
                    }
                    Ext4LockClass::ReadData => {
                        crate::utils::perf::record_ext4_read_lock(wait_ticks, hold_ticks);
                        crate::utils::perf::record_ext4_read_data_lock(wait_ticks, hold_ticks);
                    }
                    Ext4LockClass::Find => {
                        crate::utils::perf::record_ext4_find_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Fstat => {
                        crate::utils::perf::record_ext4_fstat_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Write => {
                        crate::utils::perf::record_ext4_write_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Rename => {
                        crate::utils::perf::record_ext4_rename_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Close => {
                        crate::utils::perf::record_ext4_close_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::ReadAll => {
                        crate::utils::perf::record_ext4_read_all_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::ReadDir => {
                        crate::utils::perf::record_ext4_read_dir_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::PathResolve => {
                        crate::utils::perf::record_ext4_path_resolve_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Metadata => {
                        crate::utils::perf::record_ext4_metadata_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Namespace => {
                        crate::utils::perf::record_ext4_namespace_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Sync => {
                        crate::utils::perf::record_ext4_sync_lock(wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Seek => {
                        crate::utils::perf::record_ext4_seek_lock(wait_ticks, hold_ticks)
                    }
                }
                crate::utils::perf::record_ext4_lock(wait_ticks, hold_ticks);
            }
        }
    }
}

pub(super) static EXT4_OP_LOCK: Ext4OpLock = Ext4OpLock::new();

pub use inode::*;
pub use sb::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
