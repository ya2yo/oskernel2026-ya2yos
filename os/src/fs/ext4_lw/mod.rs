//! lwext4-backed EXT4 filesystem adapter.
//!
//! The VFS adapter keeps Rust-owned descriptor and cache state per inode.
//! lwext4 owns the resource-level locks for pathname traversal, inode data,
//! block groups, superblock counters, journal state and cache mode.  Before
//! entering those C locks, however, Ya2yOS uses a task-aware mount gate so a
//! contended task sleeps instead of busy-spinning while its lock owner is
//! preempted.
//!
//! Lock order across the two layers is:
//!
//! ```text
//! VFS write_state -> VFS io_state -> EXT4_OP_LOCK -> lwext4 resource locks
//! ```
//!
//! The C layer never calls back into VFS while it holds a resource lock.
//! Rust-side dentry, inode-index and page-cache locks are short lived and are
//! released before entering lwext4 or issuing block I/O.

mod inode;
mod sb;

use alloc::collections::VecDeque;
use core::{
    future::{poll_fn, Future},
    pin::Pin,
    task::{Context, Poll, Waker},
};

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

/// One FIFO waiter for the mount-wide lwext4 admission gate.
struct Ext4OpWaiter {
    ticket: usize,
    tid: usize,
    waker: Waker,
}

/// Logical state of the mount-wide lwext4 admission gate.
///
/// This state is deliberately independent of the primitive `spin::Mutex`:
/// waiting kernel tasks are queued and put to sleep by `block_on`, rather
/// than spinning on the primitive lock while the current owner is preempted.
struct Ext4OpState {
    held: bool,
    owner_tid: usize,
    next_ticket: usize,
    waiters: VecDeque<Ext4OpWaiter>,
}

impl Ext4OpState {
    fn new() -> Self {
        Self {
            held: false,
            owner_tid: 0,
            next_ticket: 1,
            waiters: VecDeque::new(),
        }
    }
}

/// Task-aware mount-wide admission gate for all lwext4 C API calls.
///
/// lwext4's internal resource locks are raw C spin locks.  They cannot yield
/// when a lock holder is preempted, so allowing several Ya2yOS tasks to enter
/// them concurrently can deadlock all runnable harts.  This gate serializes
/// entry at the Rust boundary and gives contended task-context callers a FIFO
/// sleep queue.  Boot-time callers without a task retain a spin fallback.
pub(crate) struct Ext4OpLock {
    state: spin::Lazy<spin::Mutex<Ext4OpState>>,
}

pub(crate) struct Ext4OpGuard<'a> {
    lock: &'a Ext4OpLock,
    tid: usize,
    held: bool,
}

struct Ext4OpLockFuture<'a> {
    lock: &'a Ext4OpLock,
    tid: usize,
    ticket: Option<usize>,
}

impl Ext4OpLock {
    pub const fn new() -> Self {
        Self {
            state: spin::Lazy::new(|| spin::Mutex::new(Ext4OpState::new())),
        }
    }

    pub(crate) fn lock(&self) -> Ext4OpGuard<'_> {
        let task = crate::task::current_task();
        let tid = task.as_ref().map_or(0, |task| task.tid());
        if let Some(guard) = self.try_lock_unqueued(tid) {
            return guard;
        }

        let Some(task) = task else {
            loop {
                if let Some(guard) = self.try_lock_unqueued(0) {
                    return guard;
                }
                core::hint::spin_loop();
            }
        };

        let tid = task.tid();
        drop(task);
        crate::task::block_on(Ext4OpLockFuture {
            lock: self,
            tid,
            ticket: None,
        })
    }

    fn try_lock_unqueued(&self, tid: usize) -> Option<Ext4OpGuard<'_>> {
        let mut state = self.state.lock();
        if state.held || !state.waiters.is_empty() {
            return None;
        }
        state.held = true;
        state.owner_tid = tid;
        Some(Ext4OpGuard {
            lock: self,
            tid,
            held: true,
        })
    }

    fn release(&self, tid: usize) {
        let next = {
            let mut state = self.state.lock();
            debug_assert!(state.held, "EXT4 operation gate released while idle");
            debug_assert_eq!(
                state.owner_tid, tid,
                "EXT4 operation gate released by non-owner"
            );
            state.held = false;
            state.owner_tid = 0;
            state.waiters.front().map(|waiter| waiter.waker.clone())
        };
        if let Some(waker) = next {
            waker.wake();
        }
    }

    fn cancel_ticket(&self, ticket: usize) {
        let next = {
            let mut state = self.state.lock();
            let Some(index) = state
                .waiters
                .iter()
                .position(|waiter| waiter.ticket == ticket)
            else {
                return;
            };
            state.waiters.remove(index);
            (!state.held)
                .then(|| state.waiters.front().map(|waiter| waiter.waker.clone()))
                .flatten()
        };
        if let Some(waker) = next {
            waker.wake();
        }
    }

    /// Removes a queued waiter or releases ownership when a task leaves by a
    /// diverging scheduler path and therefore cannot run its Rust destructors.
    fn cancel_waiter_by_tid(&self, tid: usize) {
        let next = {
            let mut state = self.state.lock();
            if let Some(index) = state.waiters.iter().position(|waiter| waiter.tid == tid) {
                state.waiters.remove(index);
            }
            if state.held && state.owner_tid == tid {
                state.held = false;
                state.owner_tid = 0;
            }
            (!state.held)
                .then(|| state.waiters.front().map(|waiter| waiter.waker.clone()))
                .flatten()
        };
        if let Some(waker) = next {
            waker.wake();
        }
    }
}

impl<'a> Future for Ext4OpLockFuture<'a> {
    type Output = Ext4OpGuard<'a>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut state = this.lock.state.lock();
        let is_front = this.ticket.is_some_and(|ticket| {
            state
                .waiters
                .front()
                .is_some_and(|waiter| waiter.ticket == ticket)
        });
        let can_acquire = !state.held
            && match this.ticket {
                Some(_) => is_front,
                None => state.waiters.is_empty(),
            };
        if can_acquire {
            if this.ticket.is_some() {
                state.waiters.pop_front();
                this.ticket = None;
            }
            state.held = true;
            state.owner_tid = this.tid;
            drop(state);
            return Poll::Ready(Ext4OpGuard {
                lock: this.lock,
                tid: this.tid,
                held: true,
            });
        }

        if let Some(ticket) = this.ticket {
            if let Some(waiter) = state
                .waiters
                .iter_mut()
                .find(|waiter| waiter.ticket == ticket)
            {
                if !waiter.waker.will_wake(cx.waker()) {
                    waiter.waker = cx.waker().clone();
                }
            }
        } else {
            let ticket = state.next_ticket;
            state.next_ticket = state.next_ticket.wrapping_add(1).max(1);
            state.waiters.push_back(Ext4OpWaiter {
                ticket,
                tid: this.tid,
                waker: cx.waker().clone(),
            });
            this.ticket = Some(ticket);
        }
        Poll::Pending
    }
}

impl Drop for Ext4OpLockFuture<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            self.lock.cancel_ticket(ticket);
        }
    }
}

impl Ext4OpGuard<'_> {
    fn release(&mut self) {
        if core::mem::take(&mut self.held) {
            self.lock.release(self.tid);
        }
    }
}

impl Drop for Ext4OpGuard<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) fn cancel_ext4_op_waiter(tid: usize) {
    EXT4_OP_LOCK.cancel_waiter_by_tid(tid);
}

pub(super) static EXT4_OP_LOCK: Ext4OpLock = Ext4OpLock::new();

pub use inode::*;
pub use sb::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
