//! lwext4-backed EXT4 filesystem adapter.
//!
//! The VFS adapter keeps Rust-owned descriptor and cache state per inode.
//! lwext4 owns the resource-level locks for pathname traversal, inode data,
//! block groups, superblock counters, journal state and cache mode.  Ya2yOS
//! installs task-aware hooks for those locks, so a contended task sleeps on
//! the resource it actually needs instead of busy-spinning while its owner is
//! preempted.
//!
//! Lock order across the two layers is:
//!
//! ```text
//! VFS write_state -> VFS io_state -> lwext4 resource locks
//! ```
//!
//! The C layer never calls back into VFS while it holds a resource lock.
//! Rust-side dentry, inode-index and page-cache locks are short lived and are
//! released before entering lwext4 or issuing block I/O.

mod inode;
mod sb;

use alloc::{
    collections::{btree_map::Entry, BTreeMap, VecDeque},
    sync::Arc,
    vec::Vec,
};
use core::{
    ffi::c_void,
    future::{poll_fn, Future},
    pin::Pin,
    task::{Context, Poll, Waker},
};

#[cfg(feature = "perf")]
use crate::arch::time::get_ticks;
use crate::utils::PollSet;

extern "C" {
    fn ext4_fs_rwlock_get_kind(lock: *const c_void) -> u8;
}

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

/// Lock mode selected by the C-side lwext4 resource lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskRwLockMode {
    Read,
    Write,
}

/// One FIFO waiter for a particular lwext4 resource.  A waiter sleeps on its
/// task waker; it never spins on the C atomic word while another task owns the
/// resource.
struct TaskRwLockWaiter {
    ticket: usize,
    tid: usize,
    mode: TaskRwLockMode,
    waker: Waker,
}

struct TaskRwLockReader {
    depth: usize,
    #[cfg(feature = "perf")]
    acquired_at: usize,
}

/// State for one C `struct ext4_fs_rwlock`.  The C lock's address is the key:
/// it identifies one namespace, inode stripe, block-group stripe, journal,
/// superblock or cache resource for the lifetime of a mount.
struct TaskRwLockState {
    readers: BTreeMap<usize, TaskRwLockReader>,
    writer: Option<usize>,
    writer_depth: usize,
    #[cfg(feature = "perf")]
    writer_acquired_at: usize,
    next_ticket: usize,
    waiters: VecDeque<TaskRwLockWaiter>,
}

impl TaskRwLockState {
    fn new() -> Self {
        Self {
            readers: BTreeMap::new(),
            writer: None,
            writer_depth: 0,
            #[cfg(feature = "perf")]
            writer_acquired_at: 0,
            next_ticket: 1,
            waiters: VecDeque::new(),
        }
    }

    fn can_acquire(&self, tid: usize, mode: TaskRwLockMode, ticket: Option<usize>) -> bool {
        if let Some(owner) = self.writer {
            // lwext4's cache-write-back mode is a nesting counter.  Its
            // callers may enter the same C resource lock more than once
            // before balancing the corresponding `on_off = 0`; preserve that
            // per-task recursion without allowing another task to bypass the
            // writer.  A writer-owned task may also take a read section while
            // calling a helper that uses the same C resource; the matching
            // read unlock is tracked separately below.
            return ticket.is_none() && owner == tid;
        }

        match ticket {
            // Do not bypass a queued writer.  This keeps the lock FIFO and
            // avoids reader-induced writer starvation.
            // An already-held read lock is the exception: C wrappers such as
            // readlink -> fread reacquire namespace_lock in the same task.
            // Blocking that recursive read behind its own queued writer makes
            // the task hold the read lock forever and leaves every hart idle.
            None if mode == TaskRwLockMode::Read
                && self.readers.get(&tid).map_or(0, |reader| reader.depth) != 0 =>
            {
                true
            }
            None if !self.waiters.is_empty() => false,
            None => match mode {
                TaskRwLockMode::Read => true,
                TaskRwLockMode::Write => self.readers.is_empty(),
            },
            Some(ticket) => {
                let Some(waiter) = self.waiters.front() else {
                    return false;
                };
                if waiter.ticket != ticket || waiter.mode != mode {
                    return false;
                }
                match mode {
                    TaskRwLockMode::Read => true,
                    TaskRwLockMode::Write => self.readers.is_empty(),
                }
            }
        }
    }

    fn wake_waiters(&self) -> Vec<Waker> {
        if self.writer.is_some() {
            return Vec::new();
        }

        let mut wakers = Vec::new();
        for waiter in &self.waiters {
            match waiter.mode {
                // Readers at the head of the FIFO are mutually compatible.
                // Wake the whole contiguous read batch so they can acquire
                // the resource concurrently instead of handing it off one
                // task at a time.
                TaskRwLockMode::Read => wakers.push(waiter.waker.clone()),
                TaskRwLockMode::Write => {
                    if self.readers.is_empty() {
                        wakers.push(waiter.waker.clone());
                    }
                    break;
                }
            }
        }
        wakers
    }

    fn acquire(&mut self, tid: usize, mode: TaskRwLockMode) {
        match mode {
            TaskRwLockMode::Read => {
                if let Some(reader) = self.readers.get_mut(&tid) {
                    reader.depth = reader.depth.saturating_add(1);
                } else {
                    self.readers.insert(
                        tid,
                        TaskRwLockReader {
                            depth: 1,
                            #[cfg(feature = "perf")]
                            acquired_at: get_ticks(),
                        },
                    );
                }
            }
            TaskRwLockMode::Write => {
                if self.writer == Some(tid) {
                    self.writer_depth = self.writer_depth.saturating_add(1);
                } else {
                    debug_assert!(self.writer.is_none());
                    debug_assert!(self.readers.is_empty());
                    self.writer = Some(tid);
                    self.writer_depth = 1;
                    #[cfg(feature = "perf")]
                    {
                        self.writer_acquired_at = get_ticks();
                    }
                }
            }
        }
    }
}

/// A FIFO, task-aware rwsem backing exactly one lwext4 C resource lock.
///
/// This is deliberately not a VFS-wide lock.  Independent C lock addresses
/// have independent state, allowing different inode stripes and block groups
/// to make progress concurrently.  Boot-time code has no task to park, so it
/// retains a bounded spin fallback during early mount.
struct TaskRwLock {
    class: crate::utils::perf::Ext4ResourceLockClass,
    state: spin::Mutex<TaskRwLockState>,
}

struct TaskRwLockFuture<'a> {
    lock: &'a TaskRwLock,
    tid: usize,
    mode: TaskRwLockMode,
    ticket: Option<usize>,
    #[cfg(feature = "perf")]
    started_at: usize,
}

impl TaskRwLock {
    fn new(class: crate::utils::perf::Ext4ResourceLockClass) -> Self {
        Self {
            class,
            state: spin::Mutex::new(TaskRwLockState::new()),
        }
    }

    fn lock(&self, mode: TaskRwLockMode) {
        #[cfg(feature = "perf")]
        let started_at = get_ticks();
        let task = crate::task::current_task();
        let tid = task.as_ref().map_or(0, |task| task.tid());
        if self.try_lock(tid, mode, None) {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_ext4_resource_lock_acquired(self.class, 0, false);
            return;
        }

        let Some(task) = task else {
            loop {
                if self.try_lock(0, mode, None) {
                    #[cfg(feature = "perf")]
                    crate::utils::perf::record_ext4_resource_lock_acquired(
                        self.class,
                        get_ticks().saturating_sub(started_at),
                        true,
                    );
                    return;
                }
                core::hint::spin_loop();
            }
        };

        let tid = task.tid();
        drop(task);
        crate::task::block_on(TaskRwLockFuture {
            lock: self,
            tid,
            mode,
            ticket: None,
            #[cfg(feature = "perf")]
            started_at,
        });
    }

    fn try_lock(&self, tid: usize, mode: TaskRwLockMode, ticket: Option<usize>) -> bool {
        let next = {
            let mut state = self.state.lock();
            if !state.can_acquire(tid, mode, ticket) {
                return false;
            }
            if ticket.is_some() {
                state.waiters.pop_front();
            }
            state.acquire(tid, mode);
            state.wake_waiters()
        };
        for waker in next {
            waker.wake();
        }
        true
    }

    fn unlock(&self, tid: usize, mode: TaskRwLockMode) {
        #[cfg(feature = "perf")]
        let mut hold_ticks = None;
        let next = {
            let mut state = self.state.lock();
            match mode {
                TaskRwLockMode::Read => {
                    let remove = {
                        let held = state
                            .readers
                            .get_mut(&tid)
                            .expect("lwext4 read lock released by non-owner");
                        debug_assert!(held.depth > 0);
                        held.depth -= 1;
                        held.depth == 0
                    };
                    if remove {
                        #[cfg(feature = "perf")]
                        {
                            let started_at = state
                                .readers
                                .get(&tid)
                                .expect("lwext4 read lock lost owner timestamp")
                                .acquired_at;
                            hold_ticks = Some(get_ticks().saturating_sub(started_at));
                        }
                        state.readers.remove(&tid);
                    }
                }
                TaskRwLockMode::Write => {
                    assert_eq!(
                        state.writer,
                        Some(tid),
                        "lwext4 write lock released by non-owner"
                    );
                    debug_assert!(state.writer_depth > 0);
                    state.writer_depth -= 1;
                    if state.writer_depth == 0 {
                        #[cfg(feature = "perf")]
                        {
                            hold_ticks = Some(get_ticks().saturating_sub(state.writer_acquired_at));
                            state.writer_acquired_at = 0;
                        }
                        state.writer = None;
                    }
                }
            }
            state.wake_waiters()
        };
        for waker in next {
            waker.wake();
        }
        #[cfg(feature = "perf")]
        if let Some(hold_ticks) = hold_ticks {
            crate::utils::perf::record_ext4_resource_lock_released(self.class, hold_ticks);
        }
    }

    fn write_owned_by(&self, tid: usize) -> bool {
        self.state.lock().writer == Some(tid)
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
            state.wake_waiters()
        };
        for waker in next {
            waker.wake();
        }
    }

    /// Releases any resource lock a task abandoned on a diverging scheduler
    /// exit path and removes its pending waiters.  The normal C unlock hook
    /// handles the ordinary case; this mirrors the old gate's task-exit
    /// cleanup without reintroducing a mount-wide synchronization point.
    fn cancel_tid(&self, tid: usize) {
        let next = {
            let mut state = self.state.lock();
            state.waiters.retain(|waiter| waiter.tid != tid);
            state.readers.remove(&tid);
            if state.writer == Some(tid) {
                state.writer = None;
                state.writer_depth = 0;
                #[cfg(feature = "perf")]
                {
                    state.writer_acquired_at = 0;
                }
            }
            state.wake_waiters()
        };
        for waker in next {
            waker.wake();
        }
    }
}

impl Future for TaskRwLockFuture<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        #[cfg(feature = "perf")]
        let mut queued_depth = None;
        let next = {
            let mut state = this.lock.state.lock();
            if state.can_acquire(this.tid, this.mode, this.ticket) {
                if this.ticket.is_some() {
                    state.waiters.pop_front();
                    this.ticket = None;
                }
                state.acquire(this.tid, this.mode);
                Some(state.wake_waiters())
            } else if let Some(ticket) = this.ticket {
                if let Some(waiter) = state
                    .waiters
                    .iter_mut()
                    .find(|waiter| waiter.ticket == ticket)
                {
                    if !waiter.waker.will_wake(cx.waker()) {
                        waiter.waker = cx.waker().clone();
                    }
                }
                None
            } else {
                let ticket = state.next_ticket;
                state.next_ticket = state.next_ticket.wrapping_add(1).max(1);
                state.waiters.push_back(TaskRwLockWaiter {
                    ticket,
                    tid: this.tid,
                    mode: this.mode,
                    waker: cx.waker().clone(),
                });
                this.ticket = Some(ticket);
                #[cfg(feature = "perf")]
                {
                    queued_depth = Some(state.waiters.len());
                }
                None
            }
        };

        #[cfg(feature = "perf")]
        if let Some(queue_depth) = queued_depth {
            crate::utils::perf::record_ext4_resource_lock_queued(this.lock.class, queue_depth);
        }

        match next {
            Some(next) => {
                #[cfg(feature = "perf")]
                crate::utils::perf::record_ext4_resource_lock_acquired(
                    this.lock.class,
                    get_ticks().saturating_sub(this.started_at),
                    true,
                );
                for waker in next {
                    waker.wake();
                }
                Poll::Ready(())
            }
            None => Poll::Pending,
        }
    }
}

impl Drop for TaskRwLockFuture<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            self.lock.cancel_ticket(ticket);
        }
    }
}

/// Resource locks are allocated lazily because the C mount owns their
/// addresses.  The table lock only protects Rust bookkeeping; no C operation,
/// device I/O or wait occurs while it is held.
static LWEXT4_RESOURCE_LOCKS: spin::Lazy<spin::Mutex<BTreeMap<usize, Arc<TaskRwLock>>>> =
    spin::Lazy::new(|| spin::Mutex::new(BTreeMap::new()));

fn resource_lock(
    lock: *mut c_void,
    class: impl FnOnce() -> crate::utils::perf::Ext4ResourceLockClass,
) -> Arc<TaskRwLock> {
    assert!(!lock.is_null(), "lwext4 supplied a null resource lock");
    #[cfg(feature = "perf")]
    let started_at = get_ticks();
    let mut locks = LWEXT4_RESOURCE_LOCKS.lock();
    #[cfg(feature = "perf")]
    let mut created = false;
    let resource = match locks.entry(lock as usize) {
        Entry::Occupied(entry) => entry.get().clone(),
        Entry::Vacant(entry) => {
            #[cfg(feature = "perf")]
            {
                created = true;
            }
            entry.insert(Arc::new(TaskRwLock::new(class()))).clone()
        }
    };
    drop(locks);
    #[cfg(feature = "perf")]
    crate::utils::perf::record_ext4_resource_registry(
        get_ticks().saturating_sub(started_at),
        created,
    );
    resource
}

fn active_resource_lock(lock: *mut c_void) -> Arc<TaskRwLock> {
    assert!(!lock.is_null(), "lwext4 supplied a null resource lock");
    #[cfg(feature = "perf")]
    let started_at = get_ticks();
    let resource = LWEXT4_RESOURCE_LOCKS
        .lock()
        .get(&(lock as usize))
        .cloned()
        .expect("lwext4 resource lock released before acquisition");
    #[cfg(feature = "perf")]
    crate::utils::perf::record_ext4_resource_registry(
        get_ticks().saturating_sub(started_at),
        false,
    );
    resource
}

fn lwext4_resource_lock_class(lock: *mut c_void) -> crate::utils::perf::Ext4ResourceLockClass {
    use crate::utils::perf::Ext4ResourceLockClass;

    match unsafe { ext4_fs_rwlock_get_kind(lock.cast_const()) } {
        1 => Ext4ResourceLockClass::Namespace,
        2 => Ext4ResourceLockClass::Inode,
        3 => Ext4ResourceLockClass::Group,
        4 => Ext4ResourceLockClass::Super,
        5 => Ext4ResourceLockClass::Journal,
        6 => Ext4ResourceLockClass::CacheState,
        7 => Ext4ResourceLockClass::CacheFlush,
        _ => Ext4ResourceLockClass::Unknown,
    }
}

/// lwext4 通过已注册函数指针调用的资源加锁回调。
///
/// C 库只决定何时及以何种模式保护其内部资源；具体的任务阻塞、唤醒和
/// 读写锁实现由 Ya2yOS 提供。`write` 为 `true` 时获取写锁，否则获取读锁。
unsafe extern "C" fn lock_lwext4_resource(_ctx: *mut c_void, lock: *mut c_void, write: bool) {
    resource_lock(lock, || lwext4_resource_lock_class(lock)).lock(if write {
        TaskRwLockMode::Write
    } else {
        TaskRwLockMode::Read
    });
}

/// lwext4 通过已注册函数指针调用的资源解锁回调。
///
/// 此函数与 [`lock_lwext4_resource`] 配对，由 C 库在结束对资源的访问后
/// 调用；它按当前任务和相同的读写模式释放 Ya2yOS 管理的锁。
unsafe extern "C" fn unlock_lwext4_resource(_ctx: *mut c_void, lock: *mut c_void, write: bool) {
    let tid = crate::task::current_task()
        .as_ref()
        .map_or(0, |task| task.tid());
    active_resource_lock(lock).unlock(
        tid,
        if write {
            TaskRwLockMode::Write
        } else {
            TaskRwLockMode::Read
        },
    );
}

/// Lets C transaction cleanup distinguish its own journal scope from another
/// task's in-flight transaction without exposing the Rust lock state.
unsafe extern "C" fn lwext4_write_lock_owned_by_current(
    _ctx: *mut c_void,
    lock: *const c_void,
) -> bool {
    if lock.is_null() {
        return false;
    }
    let tid = crate::task::current_task()
        .as_ref()
        .map_or(0, |task| task.tid());
    LWEXT4_RESOURCE_LOCKS
        .lock()
        .get(&(lock as usize))
        .map_or(false, |resource| resource.write_owned_by(tid))
}

unsafe extern "C" fn lock_vfile_cache_resource(_ctx: *mut c_void, lock: *mut c_void, write: bool) {
    resource_lock(lock, || {
        crate::utils::perf::Ext4ResourceLockClass::VFileCache
    })
    .lock(if write {
        TaskRwLockMode::Write
    } else {
        TaskRwLockMode::Read
    });
}

unsafe extern "C" fn unlock_vfile_cache_resource(
    _ctx: *mut c_void,
    lock: *mut c_void,
    write: bool,
) {
    let tid = crate::task::current_task()
        .as_ref()
        .map_or(0, |task| task.tid());
    active_resource_lock(lock).unlock(
        tid,
        if write {
            TaskRwLockMode::Write
        } else {
            TaskRwLockMode::Read
        },
    );
}

/// Dynamic Rust cache entries use the same scheduler-aware lock callbacks as
/// lwext4 resources. Their addresses are short-lived, so drop the bookkeeping
/// record when the final `Arc<VFileCacheLock>` goes away.
unsafe extern "C" fn release_lwext4_resource(_ctx: *mut c_void, lock: *mut c_void) {
    if !lock.is_null() {
        LWEXT4_RESOURCE_LOCKS.lock().remove(&(lock as usize));
    }
}

/// Must run before `Ext4BlockWrapper::new()`: mount/recovery acquire the same
/// C resource locks as normal I/O and therefore need the task-aware hooks
/// from their first use.
pub(super) fn install_lwext4_resource_lock_hooks() {
    lwext4_rust::Ext4BlockWrapper::<crate::drivers::Disk>::setup_fs_rwlock_hooks(
        core::ptr::null_mut(),
        Some(lock_lwext4_resource),
        Some(unlock_lwext4_resource),
        Some(lwext4_write_lock_owned_by_current),
    );
    lwext4_rust::file::setup_vfile_cache_lock_hooks(
        core::ptr::null_mut(),
        Some(lock_vfile_cache_resource),
        Some(unlock_vfile_cache_resource),
        Some(release_lwext4_resource),
    );
}

pub(crate) fn cancel_ext4_op_waiter(tid: usize) {
    // Kept as the task-exit call-site name until task teardown is moved out of
    // this migration scope.  It now cleans only locks actually held or waited
    // on by this task; no mount-wide EXT4 operation lock exists.
    let locks: Vec<Arc<TaskRwLock>> = LWEXT4_RESOURCE_LOCKS.lock().values().cloned().collect();
    for lock in locks {
        lock.cancel_tid(tid);
    }
}

pub use inode::*;
pub use sb::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
