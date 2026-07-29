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
//!
//! ## 实际资源锁与锁序
//!
//! `EXT4_OP_LOCK` 始终是同一把挂载级 `TaskMutex`；可选的 perf 分类在
//! `crate::utils::perf` 中实现，并不引入额外的锁。当前 EXT4/VFS 数据路径中
//! 实际管理共享资源的锁如下：
//!
//! | 锁 | 粒度 | 受保护的资源和使用场景 |
//! | --- | --- | --- |
//! | `EXT4_OP_LOCK` | 挂载级、全局唯一 | lwext4 挂载块缓存及非 SMP-safe 的 path/file C API；所有 lwext4 调用都必须持有它。 |
//! | `Ext4Inode::io_state` | 每 inode | 可变 `Ext4File` descriptor、`aliases` 和延迟删除内部状态；同 inode 的 open/read、close、路径恢复不能并发改变这些状态。 |
//! | `Ext4Inode::write_state` | 每 inode | pathname、byte-cache 策略和 quota reservation 的状态转换；写入、truncate、rename、link 等与写可见性相关的操作使用它。 |
//! | `Ext4Inode::path` / `stat_cache` | 每 inode `RwLock` | VFS 侧路径镜像和可失效的 regular-file metadata cache；只保护 Rust 侧缓存，不保护 lwext4 descriptor。 |
//! | `FILE_PAGE_CACHE.pages` | 全局 `RwLock` | `(path, page_index)` 文件页缓存；命中使用读锁，发布和失效使用写锁，底层 I/O 不得在该锁内进行。 |
//! | `DENTRY_CACHE.entries`、`INODE_CACHE` | 全局 `RwLock` | VFS 的 dentry、inode identity/path 缓存；用于避免重复的路径查找和 inode 包装。 |
//! | `MNT_TABLE` / 挂载 quota | 全局或每挂载 `Mutex` | mount namespace 与容量记账；不是普通 lwext4 读路径的替代锁。 |
//!
//! 已固定且必须保持的 `TaskMutex` 锁序为：
//!
//! ```text
//! 普通 lwext4 读取：       io_state -> EXT4_OP_LOCK
//! 写入、truncate、rename： write_state -> io_state -> EXT4_OP_LOCK
//! 已驻留 byte-cache 写：   write_state -> io_state -> 不获取 EXT4_OP_LOCK
//! ```
//!
//! `read_at()` 可以在持有 `io_state` 时先短暂获取 `ReadOpen`、释放全局锁，
//! 再获取 `ReadData`；这保证 descriptor 在两段之间稳定，同时允许其他 inode
//! 使用 lwext4。禁止反向获取（例如 `EXT4_OP_LOCK -> io_state`，或
//! `io_state -> write_state`）。页缓存、dentry 和 inode-index 的 `RwLock`
//! 只做短暂查询/发布，必须在进入 lwext4 或可能阻塞的 I/O 前释放。

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
}

/// lwext4 shares one mounted block cache and does not provide SMP-safe internal
/// locking. Keep every call into its path/file API serialized until the wrapper
/// gains per-superblock concurrency support.
pub(crate) struct Ext4OpLock {
    inner: TaskMutex,
}

pub(crate) struct Ext4OpGuard<'a> {
    guard: Option<TaskMutexGuard<'a>>,
}

impl TaskMutex {
    pub const fn new() -> Self {
        Self {
            inner: spin::Mutex::new(()),
            waiters: PollSet::new(),
        }
    }

    pub fn lock(&self) -> TaskMutexGuard<'_> {
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
        TaskMutexGuard {
            lock: self,
            guard: Some(guard),
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

    pub(crate) fn lock(&self) -> Ext4OpGuard<'_> {
        let guard = self.inner.lock();
        Ext4OpGuard { guard: Some(guard) }
    }
}

impl Ext4OpGuard<'_> {
    /// Release the primitive guard before a caller performs bookkeeping.
    pub(crate) fn release(&mut self) -> bool {
        let held = self.guard.is_some();
        self.guard.take();
        held
    }
}

impl Drop for Ext4OpGuard<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

pub(super) static EXT4_OP_LOCK: Ext4OpLock = Ext4OpLock::new();

pub use inode::*;
pub use sb::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
