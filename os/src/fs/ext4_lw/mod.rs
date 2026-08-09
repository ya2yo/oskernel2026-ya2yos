//! 基于 lwext4 的 EXT4 文件系统适配器。
//!
//! VFS 适配层为每个 inode 保存由 Rust 管理的描述符和缓存状态。路径遍历、inode
//! 数据、块组、超级块计数器、日志状态和缓存模式的资源级锁由 lwext4 管理。
//! Ya2yOS 为这些锁安装支持任务调度的回调，因此发生竞争时，任务会睡眠在它
//! 实际需要的资源上，而不是在锁持有者被抢占时忙等。
//!
//! 两层之间的锁顺序为：
//!
//! ```text
//! VFS write_state -> VFS io_state -> lwext4 resource locks
//! ```
//!
//! C 层持有资源锁时不会回调 VFS。Rust 侧的 dentry、inode 索引和页缓存锁的
//! 持有时间很短，并且会在进入 lwext4 或发起块 I/O 前释放。

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
    /// 查询 lwext4 资源锁的类别编号。
    fn ext4_fs_rwlock_get_kind(lock: *const c_void) -> u8;
}

/// 用于保护单个 VFS inode 可变状态的任务感知互斥锁。
///
/// 发生竞争时，任务上下文中的调用者会睡眠而不是自旋；启动阶段没有当前任务的
/// 调用者则保留自旋回退路径。该锁保护 `Ext4File` 描述符、别名和仅由 Rust
/// 管理的延迟写状态，不会串行化无关 inode 或整个挂载的文件系统。
pub(super) struct TaskMutex {
    /// 保存受保护状态的底层自旋锁。
    inner: spin::Mutex<()>,
    /// 等待获取互斥锁的任务集合。
    waiters: PollSet,
}

/// `TaskMutex` 的持有凭证。
pub(super) struct TaskMutexGuard<'a> {
    /// 对应的互斥锁。
    lock: &'a TaskMutex,
    /// 底层锁守卫；使用 `Option` 以便在唤醒等待者前主动释放。
    guard: Option<spin::MutexGuard<'a, ()>>,
}

impl TaskMutex {
    /// 创建一个未加锁且没有等待者的任务感知互斥锁。
    pub const fn new() -> Self {
        Self {
            inner: spin::Mutex::new(()),
            waiters: PollSet::new(),
        }
    }

    /// 获取互斥锁；任务上下文中的竞争者会阻塞等待唤醒。
    pub fn lock(&self) -> TaskMutexGuard<'_> {
        let guard = match self.inner.try_lock() {
            Some(guard) => guard,
            None if crate::task::current_task().is_none() => self.inner.lock(),
            None => crate::task::block_on(poll_fn(|cx| {
                if let Some(guard) = self.inner.try_lock() {
                    return Poll::Ready(guard);
                }

                // 在第二次尝试前登记等待者，避免观察到竞争后、任务阻塞前发生解锁
                // 而导致唤醒丢失。
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
    /// 释放底层互斥锁并唤醒一个等待任务。
    fn drop(&mut self) {
        self.guard.take();
        self.lock.waiters.wake_one();
    }
}

/// C 侧 lwext4 资源锁选择的锁模式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskRwLockMode {
    /// 共享读锁。
    Read,
    /// 独占写锁。
    Write,
}

/// 某个 lwext4 资源上的一个 FIFO 等待者。
///
/// 等待者通过任务的 waker 睡眠；当其他任务持有资源时，不会在 C 原子变量上自旋。
struct TaskRwLockWaiter {
    /// 在该资源等待队列中的单调递增票号。
    ticket: usize,
    /// 等待任务的线程 ID。
    tid: usize,
    /// 请求的读写模式。
    mode: TaskRwLockMode,
    /// 资源可用时用于唤醒任务的 waker。
    waker: Waker,
}

/// 某个任务持有的可递归读锁状态。
struct TaskRwLockReader {
    /// 同一任务递归获取读锁的次数。
    depth: usize,
    #[cfg(feature = "perf")]
    /// 读锁首次获取时的时间戳。
    acquired_at: usize,
}

/// 一个 C `struct ext4_fs_rwlock` 对应的 Rust 状态。
///
/// C 锁的地址就是资源键；在一次挂载的生命周期内，它标识一个命名空间、inode
/// 分片、块组分片、日志、超级块或缓存资源。
struct TaskRwLockState {
    /// 按任务 ID 记录读锁持有者及其递归深度。
    readers: BTreeMap<usize, TaskRwLockReader>,
    /// 当前写锁持有者的任务 ID。
    writer: Option<usize>,
    /// 当前写锁的递归获取次数。
    writer_depth: usize,
    #[cfg(feature = "perf")]
    /// 写锁首次获取时的时间戳。
    writer_acquired_at: usize,
    /// 下一个等待者使用的票号。
    next_ticket: usize,
    /// 按进入顺序排列的等待队列。
    waiters: VecDeque<TaskRwLockWaiter>,
}

impl TaskRwLockState {
    /// 创建一个没有持有者和等待者的资源锁状态。
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

    /// 判断指定任务能否立即以给定模式获取锁。
    fn can_acquire(&self, tid: usize, mode: TaskRwLockMode, ticket: Option<usize>) -> bool {
        if let Some(owner) = self.writer {
            // lwext4 的回写缓存模式使用嵌套计数器。调用者可能在平衡对应的
            // `on_off = 0` 之前多次进入同一个 C 资源锁；这里保留这种按任务递归，
            // 但不允许其他任务绕过写者。持有写锁的任务也可能在调用使用同一 C
            // 资源的辅助函数时进入读区间；匹配的读解锁在下方单独跟踪。
            return ticket.is_none() && owner == tid;
        }

        match ticket {
            // 不绕过已排队的写者，以保持 FIFO 顺序并避免读者导致写者饥饿。
            // 已持有读锁的任务是例外：readlink -> fread 等 C 封装会在同一任务中
            // 重新获取 namespace_lock。如果把这次递归读锁排在自己的写者之后，
            // 该任务会永久持有读锁，并使所有 hart 都无法继续运行。
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

    /// 返回当前可以被唤醒的等待者。
    fn wake_waiters(&self) -> Vec<Waker> {
        if self.writer.is_some() {
            return Vec::new();
        }

        let mut wakers = Vec::new();
        for waiter in &self.waiters {
            match waiter.mode {
                // FIFO 队首的读者彼此兼容。唤醒连续的整批读者，使它们可以并发
                // 获取资源，而不是一次只交给一个任务。
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

    /// 记录指定任务成功获取一次锁。
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

/// 为一个 lwext4 C 资源锁提供支持的 FIFO 任务感知读写信号量。
///
/// 这不是 VFS 全局锁。不同的 C 锁地址拥有独立状态，因此不同 inode 分片和块组
/// 可以并发推进。启动阶段没有可阻塞的任务，所以早期挂载仍保留自旋回退路径。
struct TaskRwLock {
    /// 用于性能统计的资源类别。
    class: crate::utils::perf::Ext4ResourceLockClass,
    /// 保护该资源锁所有权和等待队列的内部锁。
    state: spin::Mutex<TaskRwLockState>,
}

/// 等待获取 lwext4 资源锁的 Future。
struct TaskRwLockFuture<'a> {
    /// 要获取的资源锁。
    lock: &'a TaskRwLock,
    /// 请求任务的线程 ID。
    tid: usize,
    /// 请求的读写模式。
    mode: TaskRwLockMode,
    /// 已分配的等待票号；尚未排队时为空。
    ticket: Option<usize>,
    #[cfg(feature = "perf")]
    /// 开始等待锁的时间戳。
    started_at: usize,
}

impl TaskRwLock {
    /// 创建一个尚未被任何任务持有的资源锁。
    fn new(class: crate::utils::perf::Ext4ResourceLockClass) -> Self {
        Self {
            class,
            state: spin::Mutex::new(TaskRwLockState::new()),
        }
    }

    /// 获取指定模式的资源锁；任务上下文发生竞争时会异步阻塞。
    fn lock(&self, mode: TaskRwLockMode) {
        #[cfg(feature = "perf")]
        let started_at = get_ticks();
        let task = crate::task::current_task();
        let tid = task.as_ref().map_or(0, |task| task.tid());
        if self.try_lock(tid, mode, None) {
            #[cfg(feature = "perf")]
            {
                self.record_current_task_lock_context_acquired();
                crate::utils::perf::record_ext4_resource_lock_acquired(self.class, 0, false);
            }
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

    #[cfg(feature = "perf")]
    #[inline]
    fn record_current_task_lock_context_acquired(&self) {
        if let Some(task) = crate::task::current_task() {
            task.ext4_resource_lock_acquired(self.class);
        }
    }

    #[cfg(feature = "perf")]
    #[inline]
    fn record_current_task_lock_context_released(&self) {
        if let Some(task) = crate::task::current_task() {
            task.ext4_resource_lock_released(self.class);
        }
    }

    /// 在不阻塞的情况下尝试获取资源锁。
    ///
    /// `ticket` 非空时，调用者必须同时位于等待队列队首，才能完成获取。
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

    /// 释放任务持有的一次读锁或写锁，并唤醒后续等待者。
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
        {
            self.record_current_task_lock_context_released();
            if let Some(hold_ticks) = hold_ticks {
                crate::utils::perf::record_ext4_resource_lock_released(self.class, hold_ticks);
            }
        }
    }

    /// 判断指定任务当前是否持有该资源的写锁。
    fn write_owned_by(&self, tid: usize) -> bool {
        self.state.lock().writer == Some(tid)
    }

    /// 从等待队列中移除指定票号的等待者。
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

    /// 释放任务在调度器退出分支中遗留的资源锁，并移除该任务的等待者。
    ///
    /// 普通情况由 C 解锁回调处理；此函数复现旧 gate 的任务退出清理行为，
    /// 但不会重新引入挂载范围的同步点。
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
                {
                    this.lock.record_current_task_lock_context_acquired();
                    crate::utils::perf::record_ext4_resource_lock_acquired(
                        this.lock.class,
                        get_ticks().saturating_sub(this.started_at),
                        true,
                    );
                }
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
    /// Future 被取消时，从资源等待队列中撤销其票号。
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            self.lock.cancel_ticket(ticket);
        }
    }
}

/// 资源锁采用延迟分配，因为它们的地址由 C 挂载实例所有。
///
/// 表锁只保护 Rust 侧的登记信息；持有表锁时不会执行 C 操作、设备 I/O 或等待。
static LWEXT4_RESOURCE_LOCKS: spin::Lazy<spin::Mutex<BTreeMap<usize, Arc<TaskRwLock>>>> =
    spin::Lazy::new(|| spin::Mutex::new(BTreeMap::new()));

/// 根据 C 资源锁地址查找或创建对应的任务感知锁。
///
/// `class` 仅在首次登记该地址时调用。
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

/// 查找已经登记的 C 资源锁。
///
/// 如果 C 层在未完成加锁登记前尝试解锁，则说明调用顺序违反了预期，函数会触发
/// panic。
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

/// 根据 lwext4 C 锁的类型编号转换为性能统计中的资源类别。
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

/// 让 C 事务清理逻辑区分当前任务自己的日志作用域和其他任务正在执行的事务，
/// 同时不暴露 Rust 锁状态。
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

/// 获取 lwext4 动态 vfile 缓存使用的资源锁。
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

/// 释放 lwext4 动态 vfile 缓存使用的资源锁。
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

/// 动态 Rust 缓存条目与 lwext4 资源共用支持调度的锁回调。
///
/// 这些条目的地址生命周期较短，因此最后一个 `Arc<VFileCacheLock>` 释放时，
/// 同时删除对应的登记记录。
unsafe extern "C" fn release_lwext4_resource(_ctx: *mut c_void, lock: *mut c_void) {
    if !lock.is_null() {
        LWEXT4_RESOURCE_LOCKS.lock().remove(&(lock as usize));
    }
}

/// 必须在 `Ext4BlockWrapper::new()` 之前调用。
///
/// 挂载和恢复过程会获取与普通 I/O 相同的 C 资源锁，因此从第一次使用开始就需要
/// 任务感知的锁回调。
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

/// 清理指定任务遗留的 EXT4 资源锁持有状态和等待项。
pub(crate) fn cancel_ext4_op_waiter(task: &crate::task::TaskControlBlock) {
    // 在任务清理逻辑迁出本次迁移范围前，保留任务退出调用点使用的名称。
    // 现在只清理该任务实际持有或等待的锁；不存在挂载范围的 EXT4 操作锁。
    let tid = task.tid();
    let locks: Vec<Arc<TaskRwLock>> = LWEXT4_RESOURCE_LOCKS.lock().values().cloned().collect();
    for lock in locks {
        lock.cancel_tid(tid);
    }
    #[cfg(feature = "perf")]
    task.clear_ext4_resource_lock_context();
}

pub use inode::*;
pub use sb::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
