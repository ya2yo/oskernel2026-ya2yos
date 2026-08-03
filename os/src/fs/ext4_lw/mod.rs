//! lwext4-backed EXT4 文件系统适配层。
//!
//! 该模块把第三方 `lwext4_rust` wrapper 暴露的路径式文件系统接口适配成
//! Ya2yOS 内部的 VFS trait：
//!
//! - [`inode`]：将 `Ext4File` 包装成 VFS [`Inode`](crate::fs::Inode)，并按
//!   状态、I/O、命名空间、查找、元数据和 trait 适配层拆分实现。
//! - [`sb`]：维护全局超级块、挂载点状态和块设备读写回调。
//!
//! 上层 syscall/VFS 只依赖 `superblock_*` helper 和 `Inode` trait，不直接接触
//! lwext4 的 C 风格接口。
//!
//! ## 实际资源锁与锁序
//!
//! `EXT4_OP_LOCK` 始终是同一把挂载级公平 gate；可选的 perf 分类在
//! `crate::utils::perf` 中实现，并不引入额外的资源锁。当前 EXT4/VFS 数据路径中
//! 实际管理共享资源的锁如下：
//!
//! | 锁 | 粒度 | 受保护的资源和使用场景 |
//! | --- | --- | --- |
//! | `EXT4_OP_LOCK` | 挂载级、全局唯一 | lwext4 挂载块缓存及非 SMP-safe 的 path/file C API；所有 lwext4 调用都必须持有它。 |
//! | `Ext4Inode::io_state` | 每 inode | 可变 `Ext4File` descriptor、`aliases` 和延迟删除内部状态；同 inode 的 open/read、close、路径恢复不能并发改变这些状态。 |
//! | `Ext4Inode::write_state` | 每 inode | pathname、byte-cache 策略和 quota reservation 的状态转换；写入、truncate、rename、link 等与写可见性相关的操作使用它。 |
//! | `Ext4Inode::path` / `stat_cache` | 每 inode `RwLock` | VFS 侧路径镜像、可失效的 regular-file metadata cache 及一次性 directory lookup stat；只保护 Rust 侧缓存，不保护 lwext4 descriptor。 |
//! | `FILE_PAGE_CACHE.pages` | 全局 `RwLock` | `(path, page_index)` 文件页缓存；命中使用读锁，发布和失效使用写锁，底层 I/O 不得在该锁内进行。 |
//! | `DENTRY_CACHE.entries`、`INODE_CACHE` | 全局 `RwLock` | VFS 的 dentry、inode identity/path 缓存；用于避免重复的路径查找和 inode 包装。 |
//! | `MNT_TABLE` / 挂载 quota | 全局或每挂载 `Mutex` | mount namespace 与容量记账；不是普通 lwext4 读路径的替代锁。 |
//!
//! 已固定且必须保持的锁序为：
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

use alloc::{collections::VecDeque, vec::Vec};
use core::{
    future::{poll_fn, Future},
    pin::Pin,
    task::{Context, Poll, Waker},
};

use crate::utils::PollSet;

/// A task-aware mutex for filesystem operations that can block on I/O.
///
/// Contended task-context callers sleep instead of spinning, while boot-time
/// callers without a current task retain the spin fallback. It protects the
/// per-inode state that must stay stable while a cache-only write bypasses the
/// separately fair mount-wide gate.
pub(super) struct TaskMutex {
    inner: spin::Mutex<()>,
    waiters: PollSet,
}

pub(super) struct TaskMutexGuard<'a> {
    lock: &'a TaskMutex,
    guard: Option<spin::MutexGuard<'a, ()>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Ext4OpMode {
    Shared,
    Exclusive,
}

struct Ext4OpWaiter {
    ticket: usize,
    tid: usize,
    mode: Ext4OpMode,
    queued_at: usize,
    waker: Waker,
}

struct Ext4OpReader {
    tid: usize,
    count: usize,
}

struct Ext4OpState {
    writer_held: bool,
    // A task that exits through the diverging scheduler path cannot run the
    // guard destructor. Keep the writer owner and reader ownership in the
    // logical state so exit cleanup cannot strand a queued writer forever.
    owner_tid: usize,
    active_readers: usize,
    readers: Vec<Ext4OpReader>,
    next_ticket: usize,
    waiters: VecDeque<Ext4OpWaiter>,
}

/// The lwext4 slow path needs a mount-wide admission gate while its write-side
/// allocator, namespace and journal state remain serialized.  The bcache and
/// position-independent block I/O are now safe for concurrent read-only
/// requests, so the gate admits a FIFO shared-reader batch ahead of the next
/// writer.  Once any writer queues, later readers cannot barge ahead of it.
pub(crate) struct Ext4OpLock {
    state: spin::Lazy<spin::Mutex<Ext4OpState>>,
}

pub(crate) struct Ext4OpGuard<'a> {
    lock: &'a Ext4OpLock,
    tid: usize,
    mode: Ext4OpMode,
    held: bool,
}

struct Ext4OpLockFuture<'a> {
    lock: &'a Ext4OpLock,
    tid: usize,
    mode: Ext4OpMode,
    ticket: Option<usize>,
    barging_prevented: bool,
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

impl Ext4OpState {
    fn new() -> Self {
        Self {
            writer_held: false,
            owner_tid: 0,
            active_readers: 0,
            readers: Vec::new(),
            next_ticket: 1,
            waiters: VecDeque::new(),
        }
    }

    fn add_reader(&mut self, tid: usize) {
        self.active_readers += 1;
        if let Some(reader) = self.readers.iter_mut().find(|reader| reader.tid == tid) {
            reader.count += 1;
        } else {
            self.readers.push(Ext4OpReader { tid, count: 1 });
        }
    }

    fn remove_reader(&mut self, tid: usize) -> usize {
        let index = self
            .readers
            .iter()
            .position(|reader| reader.tid == tid)
            .expect("EXT4 shared gate released by a non-reader");
        let reader = &mut self.readers[index];
        debug_assert_ne!(reader.count, 0);
        reader.count -= 1;
        self.active_readers -= 1;
        if reader.count == 0 {
            self.readers.remove(index);
        }
        self.active_readers
    }

    fn remove_readers_by_tid(&mut self, tid: usize) -> usize {
        let Some(index) = self.readers.iter().position(|reader| reader.tid == tid) else {
            return 0;
        };
        let count = self.readers.remove(index).count;
        self.active_readers -= count;
        count
    }

    fn ready_wakers(&self) -> Vec<Waker> {
        if self.writer_held || self.waiters.is_empty() {
            return Vec::new();
        }

        if self
            .waiters
            .front()
            .is_some_and(|waiter| waiter.mode == Ext4OpMode::Exclusive)
        {
            return (self.active_readers == 0)
                .then(|| self.waiters.front().unwrap().waker.clone())
                .into_iter()
                .collect();
        }

        self.waiters
            .iter()
            .take_while(|waiter| waiter.mode == Ext4OpMode::Shared)
            .map(|waiter| waiter.waker.clone())
            .collect()
    }
}

impl Ext4OpLock {
    pub const fn new() -> Self {
        Self {
            state: spin::Lazy::new(|| spin::Mutex::new(Ext4OpState::new())),
        }
    }

    pub(crate) fn lock(&self) -> Ext4OpGuard<'_> {
        self.lock_mode(Ext4OpMode::Exclusive)
    }

    pub(crate) fn lock_shared(&self) -> Ext4OpGuard<'_> {
        self.lock_mode(Ext4OpMode::Shared)
    }

    fn lock_mode(&self, mode: Ext4OpMode) -> Ext4OpGuard<'_> {
        let task = crate::task::current_task();
        let tid = task.as_ref().map_or(0, |task| task.tid());
        let (guard, reserved) = self.try_lock_unqueued(tid, mode);
        if let Some(guard) = guard {
            return guard;
        }

        let Some(task) = task else {
            loop {
                let (guard, _) = self.try_lock_unqueued(0, mode);
                if let Some(guard) = guard {
                    return guard;
                }
                core::hint::spin_loop();
            }
        };

        #[cfg(not(feature = "perf"))]
        let _ = reserved;

        let tid = task.tid();
        drop(task);
        crate::task::block_on(Ext4OpLockFuture {
            lock: self,
            tid,
            mode,
            ticket: None,
            barging_prevented: reserved,
        })
    }

    fn try_lock_unqueued(&self, tid: usize, mode: Ext4OpMode) -> (Option<Ext4OpGuard<'_>>, bool) {
        let mut state = self.state.lock();
        let can_acquire = match mode {
            Ext4OpMode::Shared => !state.writer_held && state.waiters.is_empty(),
            Ext4OpMode::Exclusive => {
                !state.writer_held && state.active_readers == 0 && state.waiters.is_empty()
            }
        };
        if can_acquire {
            let active_readers = match mode {
                Ext4OpMode::Shared => {
                    state.add_reader(tid);
                    state.active_readers
                }
                Ext4OpMode::Exclusive => {
                    state.writer_held = true;
                    state.owner_tid = tid;
                    0
                }
            };
            drop(state);
            #[cfg(not(feature = "perf"))]
            let _ = active_readers;
            #[cfg(feature = "perf")]
            match mode {
                Ext4OpMode::Shared => {
                    crate::utils::perf::record_ext4_gate_shared_acquired(active_readers)
                }
                Ext4OpMode::Exclusive => crate::utils::perf::record_ext4_gate_acquired(tid),
            }
            return (
                Some(Ext4OpGuard {
                    lock: self,
                    tid,
                    mode,
                    held: true,
                }),
                false,
            );
        }
        let reserved = !state.writer_held && !state.waiters.is_empty();
        (None, reserved)
    }

    fn release(&self, tid: usize, mode: Ext4OpMode) {
        let (active_readers, next) = {
            let mut state = self.state.lock();
            let active_readers = match mode {
                Ext4OpMode::Shared => state.remove_reader(tid),
                Ext4OpMode::Exclusive => {
                    debug_assert!(state.writer_held, "EXT4 operation gate released while idle");
                    debug_assert_eq!(
                        state.owner_tid, tid,
                        "EXT4 operation gate released by non-owner"
                    );
                    state.writer_held = false;
                    state.owner_tid = 0;
                    0
                }
            };
            #[cfg(feature = "perf")]
            if mode == Ext4OpMode::Exclusive {
                crate::utils::perf::record_ext4_gate_released();
            }
            (active_readers, state.ready_wakers())
        };
        #[cfg(feature = "perf")]
        if mode == Ext4OpMode::Shared {
            crate::utils::perf::record_ext4_gate_shared_released(active_readers);
        }
        #[cfg(not(feature = "perf"))]
        let _ = active_readers;
        Self::wake_waiters(next);
    }

    fn wake_waiters(wakers: Vec<Waker>) {
        for waker in wakers {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_gate_handoff_wake();
            waker.wake();
        }
    }

    fn cancel_ticket(&self, ticket: usize) {
        let (removed, next) = {
            let mut state = self.state.lock();
            let Some(index) = state
                .waiters
                .iter()
                .position(|waiter| waiter.ticket == ticket)
            else {
                return;
            };
            state.waiters.remove(index);
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_gate_queue_depth(state.waiters.len());
            (true, state.ready_wakers())
        };
        if removed {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_gate_waiter_cancelled(1);
        }
        Self::wake_waiters(next);
    }

    fn cancel_waiter_by_tid(&self, tid: usize) {
        let (removed, reader_releases, active_readers, next) = {
            let mut state = self.state.lock();
            let before = state.waiters.len();
            state.waiters.retain(|waiter| waiter.tid != tid);
            let removed = before.saturating_sub(state.waiters.len());
            let owner_released = state.writer_held && state.owner_tid == tid;
            if owner_released {
                state.writer_held = false;
                state.owner_tid = 0;
                #[cfg(feature = "perf")]
                crate::utils::perf::record_ext4_gate_owner_released_on_exit();
            }
            let reader_releases = state.remove_readers_by_tid(tid);
            let active_readers = state.active_readers;
            #[cfg(feature = "perf")]
            if removed != 0 {
                crate::utils::perf::record_ext4_gate_queue_depth(state.waiters.len());
            }
            (
                removed,
                reader_releases,
                active_readers,
                state.ready_wakers(),
            )
        };
        if removed != 0 {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_gate_waiter_cancelled(removed);
        }
        #[cfg(feature = "perf")]
        for _ in 0..reader_releases {
            crate::utils::perf::record_ext4_gate_shared_released(active_readers);
        }
        #[cfg(not(feature = "perf"))]
        let _ = (reader_releases, active_readers);
        Self::wake_waiters(next);
    }
}

impl<'a> Future for Ext4OpLockFuture<'a> {
    type Output = Ext4OpGuard<'a>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut state = this.lock.state.lock();
        let queued_index = this.ticket.and_then(|ticket| {
            state
                .waiters
                .iter()
                .position(|waiter| waiter.ticket == ticket)
        });
        let direct_acquire = this.ticket.is_none()
            && state.waiters.is_empty()
            && !state.writer_held
            && (this.mode == Ext4OpMode::Shared || state.active_readers == 0);
        let queued_acquire = match (this.mode, queued_index) {
            (Ext4OpMode::Shared, Some(index)) => {
                !state.writer_held
                    && state
                        .waiters
                        .iter()
                        .take(index)
                        .all(|waiter| waiter.mode == Ext4OpMode::Shared)
            }
            (Ext4OpMode::Exclusive, Some(0)) => !state.writer_held && state.active_readers == 0,
            _ => false,
        };

        if direct_acquire || queued_acquire {
            let queued_at = queued_index.map(|index| {
                let waiter = state.waiters.remove(index).unwrap();
                debug_assert_eq!(Some(waiter.ticket), this.ticket);
                waiter.queued_at
            });
            let active_readers = match this.mode {
                Ext4OpMode::Shared => {
                    state.add_reader(this.tid);
                    state.active_readers
                }
                Ext4OpMode::Exclusive => {
                    state.writer_held = true;
                    state.owner_tid = this.tid;
                    0
                }
            };
            let depth = state.waiters.len();
            let next = state.ready_wakers();
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_gate_queue_depth(depth);
            this.ticket = None;
            drop(state);

            #[cfg(feature = "perf")]
            {
                if this.barging_prevented {
                    crate::utils::perf::record_ext4_gate_barging_prevented();
                    this.barging_prevented = false;
                }
                if let Some(queued_at) = queued_at {
                    let wait_ticks = crate::arch::time::get_ticks().saturating_sub(queued_at);
                    match this.mode {
                        Ext4OpMode::Shared => {
                            crate::utils::perf::record_ext4_gate_shared_handoff_acquire(
                                wait_ticks,
                                active_readers,
                            )
                        }
                        Ext4OpMode::Exclusive => {
                            crate::utils::perf::record_ext4_gate_handoff_acquire(
                                this.tid, wait_ticks,
                            )
                        }
                    }
                } else {
                    match this.mode {
                        Ext4OpMode::Shared => {
                            crate::utils::perf::record_ext4_gate_shared_acquired(active_readers)
                        }
                        Ext4OpMode::Exclusive => {
                            crate::utils::perf::record_ext4_gate_acquired(this.tid)
                        }
                    }
                }
            }
            #[cfg(not(feature = "perf"))]
            let _ = (queued_at, active_readers, depth);

            Ext4OpLock::wake_waiters(next);

            return Poll::Ready(Ext4OpGuard {
                lock: this.lock,
                tid: this.tid,
                mode: this.mode,
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
            this.barging_prevented |= !state.writer_held && !state.waiters.is_empty();
            let ticket = state.next_ticket;
            state.next_ticket = state.next_ticket.wrapping_add(1).max(1);
            state.waiters.push_back(Ext4OpWaiter {
                ticket,
                tid: this.tid,
                mode: this.mode,
                #[cfg(feature = "perf")]
                queued_at: crate::arch::time::get_ticks(),
                #[cfg(not(feature = "perf"))]
                queued_at: 0,
                waker: cx.waker().clone(),
            });
            this.ticket = Some(ticket);
            let depth = state.waiters.len();
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_gate_queue_depth(depth);
            drop(state);
            #[cfg(feature = "perf")]
            {
                crate::utils::perf::record_ext4_gate_queued();
                if this.barging_prevented {
                    crate::utils::perf::record_ext4_gate_barging_prevented();
                    this.barging_prevented = false;
                }
            }
            #[cfg(not(feature = "perf"))]
            let _ = depth;
            return Poll::Pending;
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
    /// Release the logical gate before a caller performs bookkeeping.
    pub(crate) fn release(&mut self) -> bool {
        let held = core::mem::take(&mut self.held);
        if held {
            self.lock.release(self.tid, self.mode);
        }
        held
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
