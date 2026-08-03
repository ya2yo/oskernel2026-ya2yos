//! lwext4-backed EXT4 filesystem adapter.
//!
//! The VFS adapter keeps Rust-owned descriptor and cache state per inode.
//! lwext4 itself now owns SMP synchronization for pathname traversal,
//! inode data, block groups, superblock counters, journal state and cache
//! mode.  There is intentionally no mount-wide Rust operation gate.
//!
//! Lock order across the two layers is:
//!
//! ```text
//! VFS write_state -> VFS io_state -> lwext4 namespace/inode/group/journal
//! ```
//!
//! The C layer never calls back into VFS while it holds a resource lock.
//! Rust-side dentry, inode-index and page-cache locks are short lived and are
//! released before entering lwext4 or issuing block I/O.

mod inode;
mod sb;

use core::{future::poll_fn, task::Poll};

use crate::utils::PollSet;

/// A task-aware mutex for mutable state of one VFS inode.
///
/// Contended task-context callers sleep instead of spinning, while boot-time
/// callers without a current task retain the spin fallback.  It protects an
/// `Ext4File` descriptor, aliases and Rust-only delayed-write state; it does
/// not serialize unrelated inodes or the mounted filesystem.
pub(super) struct TaskMutex {
    inner: spin::Mutex<()>,
    waiters: PollSet,
}

pub(super) struct TaskMutexGuard<'a> {
    lock: &'a TaskMutex,
    guard: Option<spin::MutexGuard<'a, ()>>,
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
        self.guard.take();
        self.lock.waiters.wake_one();
    }
}

pub use inode::*;
pub use sb::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
