//! Futex（fast userspace mutex）系统调用及其等待队列实现。
//!
//! Futex 的核心约定是：用户空间先通过原子操作尝试改变锁状态，只有在
//! 竞争失败时才进入内核等待。内核不会替用户空间维护锁本身，而是负责
//! 将等待者按 futex key 排队、在匹配的 `WAKE`/`WAKE_BITSET` 到来时唤醒，
//! 以及在超时、信号或线程退出时清理等待状态。
//!
//! 本实现同时覆盖以下几类行为：
//! - `FUTEX_WAIT`/`FUTEX_WAIT_BITSET` 的比较后阻塞；
//! - `FUTEX_WAKE`/`FUTEX_WAKE_BITSET` 的按位集合唤醒；
//! - `FUTEX_REQUEUE` 的唤醒与等待者迁移；
//! - robust futex 在线程退出时的 `OWNER_DIED` 标记与唤醒。
//!
//! 队列 key 对私有 futex 使用页表 token 与用户地址的组合，对共享 futex
//! 使用翻译后的物理地址，从而区分不同地址空间中的同名用户地址，同时
//! 允许共享映射在不同进程间找到同一组等待者。

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    mm::{
        copy_from_user, copy_from_user_val, copy_to_user_val, translate_user_va_safe,
        try_copy_from_user_val, MemorySet, VirtAddr,
    },
    syscall::{FutexCmd, FutexOpt},
    task::RobustListHead,
    timer::{add_futex_timer, get_time_spec, Timespec},
    utils::{SysErrNo, SyscallRet},
};

use super::{
    current_task, requeue_futex_task, schedule_blocked_current, timeout_futex_task,
    wakeup_futex_task, TaskControlBlock, TaskStatus,
};
use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
};
use log::{debug, error};
use spin::Lazy;

use crate::sync::RemoteTlbMutex;

// ------------------------- robust futex constants ------------------------
/// Bit 31：有等待者正在该 futex 上睡眠。
const FUTEX_WAITERS: u32 = 0x80000000;
/// Bit 30：原 owner 在持有该 futex 期间退出。
const FUTEX_OWNER_DIED: u32 = 0x40000000;
/// Bits 0..29：线程 id。
const FUTEX_TID_MASK: u32 = 0x3fffffff;
/// 遍历 robust list 时允许的最大节点数，防止链表损坏导致死循环。
const ROBUST_LIST_LIMIT: usize = 2048;

// ------------------------- type defs --------------------------------

/// 一个等待 futex 的线程及其一次等待操作的标识。
///
/// `task` 使用弱引用，避免线程已经退出而队列仍残留时延长 TCB
/// 生命周期。`futex_key` 不是用户地址，而是本次等待的唯一代数；
/// 唤醒和超时都必须携带它，以免旧定时器误伤线程后续复用的等待。
pub struct FutexWaiter {
    pub task: Weak<TaskControlBlock>,
    /// `FUTEX_WAIT_BITSET` 使用的匹配掩码；普通 `FUTEX_WAIT` 为全 1。
    pub bitset: u32,
    /// 本次等待的唯一 key，用于与唤醒/定时器操作配对。
    pub futex_key: usize,
}

/// 同一个 futex key 上按 FIFO 顺序排列的等待者。
type BitsetWaitQueue = VecDeque<FutexWaiter>;

/// futex key 到等待队列的映射。
///
/// 锁的持有范围覆盖 waiter 的插入、移除和状态发布，用于实现
/// compare-and-block 的线性化顺序。`RemoteTlbMutex` 还允许在处理远程
/// TLB 相关路径时安全地访问这张全局表。
pub static FUTEX_QUEUE_BITMAP: Lazy<RemoteTlbMutex<BTreeMap<usize, BitsetWaitQueue>>> =
    Lazy::new(|| RemoteTlbMutex::new(BTreeMap::new()));
/// 等待队列发生变化的版本号，用来检测检查用户值与入队之间的并发唤醒。
static FUTEX_QUEUE_VERSION: AtomicUsize = AtomicUsize::new(0);

/// 发布一次等待队列变更。
///
/// 等待者在读取用户态 futex 值后、真正入队前会记住该版本；如果期间
/// 有唤醒者修改队列，等待者会重新检查用户值并重试，从而避免丢失唤醒。
#[inline]
fn bump_futex_queue_version() {
    FUTEX_QUEUE_VERSION.fetch_add(1, Ordering::AcqRel);
}

/// 唤醒指定 futex key 上的等待线程。
///
/// 这是传统 `FUTEX_WAKE` 的便捷入口，等价于使用全 1 bitset 的
/// `futex_wake_up_bitset`。返回实际成功从阻塞态转为可运行态的线程数。
pub fn futex_wake_up(pa: usize, max_num: i32) -> usize {
    // 重定向需求
    return futex_wake_up_bitset(pa, max_num, u32::MAX);
}

/// 在 robust futex 的用户地址上唤醒等待者。
///
/// robust list 只保存用户虚拟地址，因此先尝试使用当前地址空间翻译出的
/// 物理地址唤醒共享 futex，再使用“地址空间 token ^ 用户地址”唤醒私有
/// futex。两个 key 都尝试是必要的，因为退出处理并不知道 futex 的私有/共享
/// 属性。
fn futex_wake_robust_user_addr(memory_set: &MemorySet, uaddr: usize, max_num: i32) -> usize {
    let mut woken = 0;
    if let Ok(pa) = translate_user_va_safe(memory_set, VirtAddr::from(uaddr)) {
        woken += futex_wake_up_bitset(pa, max_num, u32::MAX);
    }
    woken += futex_wake_up_bitset(memory_set.token() ^ uaddr, max_num, u32::MAX);
    woken
}

/// 唤醒旧 key 上的部分线程，并将其余等待者迁移到新 key。
///
/// `max_wakeup` 限制直接唤醒的数量，`max_requeue` 限制迁移的数量；
/// 被迁移的线程仍然处于阻塞态，因此返回值只统计直接唤醒的线程。这一
/// 语义是 pthread 条件变量实现依赖的：调用方据此判断是否需要继续发出
/// 唤醒，而不会把已经 requeue 的线程误认为已完成唤醒。
fn futex_requeue(old_pa: usize, max_wakeup: i32, new_pa: usize, max_requeue: i32) -> usize {
    // log::debug!(
    //     "[futex_requeue],old_key={:?},max_wakeup={},new_key={:?},max_requeue={}",
    //     old_pa,
    //     max_wakeup,
    //     new_pa,
    //     max_requeue
    // );
    bump_futex_queue_version();
    let wake_limit = max_wakeup.max(0) as usize;
    let requeue_limit = max_requeue.max(0) as usize;
    let mut futex_queue = FUTEX_QUEUE_BITMAP.lock();
    let Some(mut old_queue) = futex_queue.remove(&old_pa) else {
        return 0;
    };
    let mut woken = 0;
    let mut moved = VecDeque::new();
    let mut requeued = 0;
    let mut retained = VecDeque::new();
    while let Some(waiter) = old_queue.pop_front() {
        let Some(task) = waiter.task.upgrade() else {
            continue;
        };
        if woken < wake_limit {
            if wakeup_futex_task(task, waiter.futex_key) {
                woken += 1;
            }
        } else if requeued < requeue_limit {
            if requeue_futex_task(&task, waiter.futex_key, new_pa) {
                moved.push_back(waiter);
                requeued += 1;
            }
        } else {
            // Entries beyond the requested requeue count remain waiters on
            // the original futex.  Dropping them loses a blocking thread.
            retained.push_back(waiter);
        }
    }

    if old_pa == new_pa {
        retained.extend(moved);
        if !retained.is_empty() {
            futex_queue.insert(old_pa, retained);
        }
    } else {
        if !retained.is_empty() {
            futex_queue.insert(old_pa, retained);
        }
        if !moved.is_empty() {
            futex_queue.entry(new_pa).or_default().extend(moved);
        }
    }
    // FUTEX_REQUEUE reports only the number of waiters woken directly.
    // Requeued waiters remain blocked on the destination futex and must not
    // be counted as successful wakeups by pthread condition-variable code.
    woken
}

/// Queue a waiter only while its expected user-space value still matches.
///
/// `FUTEX_QUEUE_BITMAP` is this implementation's counterpart to Linux's
/// futex hash-bucket lock.  User memory is checked before taking the queue
/// lock, while `FUTEX_QUEUE_VERSION` closes the race with a waker that enters
/// the queue lock between the check and waiter insertion.  This keeps the
/// queue lock out of the `MemorySet` read path while preserving the
/// compare-and-block ordering.
/// 通过用户态比较值实现 futex 的“比较并阻塞”操作。
///
/// 函数只有在 `*uaddr == expected` 时才会登记 waiter；如果值已经变化，
/// 立即返回 `EAGAIN`。登记后线程发布为 `Blocked`，再安装可选定时器并
/// 切出当前 CPU。恢复运行后统一检查信号、超时和正常唤醒结果，并移除
/// 可能遗留的队列项。
///
/// `bitset` 决定该 waiter 可被哪些 `FUTEX_WAKE_BITSET` 匹配；普通 wait
/// 使用全 1 掩码。`timeout` 已由系统调用层转换为内核使用的绝对时间。
fn futex_wait_bitset(
    queue_key: usize,
    task: Arc<TaskControlBlock>,
    memory_set: &MemorySet,
    uaddr: *const i32,
    expected: i32,
    bitset: u32,
    timeout: Option<Timespec>,
) -> SyscallRet {
    #[cfg(feature = "perf")]
    let active_guard = crate::utils::perf::FutexActiveGuard::new();

    let futex_key = new_futex_key();
    let task_cx_ptr = loop {
        let current_val: i32 = copy_from_user_val(memory_set, uaddr)?;
        if current_val != expected {
            return Err(SysErrNo::EAGAIN);
        }
        let version = FUTEX_QUEUE_VERSION.load(Ordering::Acquire);
        let mut waitq = FUTEX_QUEUE_BITMAP.lock();
        if FUTEX_QUEUE_VERSION.load(Ordering::Acquire) != version {
            drop(waitq);
            continue;
        }

        let task_cx_ptr = {
            let mut inner = task.inner_lock();
            // 与 Linux futex 等待一致：在 bucket 锁下完成信号复查、waiter
            // 登记和睡眠态发布；on_cpu 在真正切出前阻止唤醒者重新调度。
            if !inner.sig_pending.difference(inner.sig_mask).is_empty() {
                return Err(SysErrNo::EINTR);
            }

            inner.futex_timedout = false;
            inner.futex_pa = queue_key;
            inner.futex_key = futex_key;
            waitq.entry(queue_key).or_default().push_back(FutexWaiter {
                task: Arc::downgrade(&task),
                bitset,
                futex_key,
            });
            inner.task_status = TaskStatus::Blocked;
            &mut inner.task_cx as *mut _
        };
        drop(waitq);
        break task_cx_ptr;
    };

    // `handle_timer()` takes the timer lock before the futex queue lock, so
    // install the timer only after releasing the queue lock.  The waiter is
    // already visible and Blocked, hence an immediate wake or timeout remains
    // safe.
    if let Some(timeout) = timeout {
        add_futex_timer(timeout, &task, futex_key);
    }
    drop(task);
    #[cfg(feature = "perf")]
    drop(active_guard);
    schedule_blocked_current(task_cx_ptr);
    #[cfg(feature = "perf")]
    let _resume_active_guard = crate::utils::perf::FutexActiveGuard::new();
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();

    // 1) 因信号唤醒 → EINTR
    if !task_inner
        .sig_pending
        .difference(task_inner.sig_mask)
        .is_empty()
    {
        let futex_key = task_inner.futex_key;
        let futex_pa = task_inner.futex_pa;
        drop(task_inner);
        if futex_key != 0 {
            bump_futex_queue_version();
            let mut waitq = FUTEX_QUEUE_BITMAP.lock();
            if let Some(queue) = waitq.get_mut(&futex_pa) {
                if let Some(idx) = queue.iter().position(|x| x.futex_key == futex_key) {
                    queue.remove(idx);
                }
            }
        }
        return Err(SysErrNo::EINTR);
    }

    // 2) 因超时唤醒 → ETIMEDOUT
    if task_inner.futex_timedout {
        let futex_key = task_inner.futex_key;
        let futex_pa = task_inner.futex_pa;
        task_inner.futex_timedout = false;
        drop(task_inner);
        // 超时定时器已通过 handle_timer 将 waiter 摘下并设置了 timedout 标记；
        // futex_key 在 wakeup_futex_task 里已被清 0，此处是安全网。
        if futex_key != 0 {
            bump_futex_queue_version();
            let mut waitq = FUTEX_QUEUE_BITMAP.lock();
            if let Some(queue) = waitq.get_mut(&futex_pa) {
                if let Some(idx) = queue.iter().position(|x| x.futex_key == futex_key) {
                    queue.remove(idx);
                }
            }
        }
        return Err(SysErrNo::ETIMEDOUT);
    }

    drop(task_inner);
    Ok(0)
}

/// 按 bitset 从指定队列唤醒最多 `max_num` 个等待者。
///
/// 不匹配当前 bitset 的 waiter 会被放回队尾，使一次唤醒不会破坏其他
/// bitset 等待者的 FIFO 顺序。返回值只包含真正成功唤醒的线程；空 bitset
/// 不会匹配任何 waiter。
fn futex_wake_up_bitset(pa: usize, max_num: i32, bitset: u32) -> usize {
    if bitset == 0 {
        return 0;
    }
    // Keep the traditional futex(2) ABI behavior.  Linux's legacy FUTEX_WAKE
    // path treats a nonpositive count as one wake; converting a negative
    // `val` directly to usize would instead wake every queued task here.
    let max_num = if max_num <= 0 { 1 } else { max_num as usize };
    // log::debug!(
    //     "[sys_futex] futex wakeup thread,max_num={},key={:?}",
    //     max_num,
    //     pa
    // );
    bump_futex_queue_version();
    let mut futex_queue = FUTEX_QUEUE_BITMAP.lock();
    let mut num: usize = 0;
    if let Some(queue) = futex_queue.get_mut(&pa) {
        let queue_len = queue.len();
        // 我们会遍历这个deque，最多len次
        let mut cnt: usize = 0;
        while cnt < queue_len && num < max_num {
            cnt += 1;
            if let Some(waiter) = queue.pop_front() {
                if let Some(task) = waiter.task.upgrade() {
                    // 需要检查：是不是确实相交不为0

                    if bitset & waiter.bitset != 0 {
                        if wakeup_futex_task(task, waiter.futex_key) {
                            num += 1;
                        }
                    } else {
                        // 我还得给它还回去
                        // TODO: 这里也许可以做性能优化？
                        queue.push_back(waiter);
                    }
                }
            } else {
                // 队列空！
                break;
            }
        }
    }
    // debug!("futex_wake_up_bitset: wake {} threads", num);
    num
}

static FUTEX_KEY_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// 为一次 futex wait 分配唯一标识。
///
/// 0 保留给“没有等待/已清理”的状态，因此计数器溢出后也不会把首次
/// 返回值设为 0；在实际地址空间与线程生命周期范围内，key 只用于区分
/// 相邻等待代数，不承诺永久不重复。
fn new_futex_key() -> usize {
    // +1 确保 key 从 1 开始，0 表示"无/已清理的 Waiter"
    FUTEX_KEY_COUNTER.fetch_add(1, Ordering::Relaxed) + 1
}

/// 根据 futex 是否为私有 futex 计算等待队列 key。
///
/// 私有 futex 的语义限定在当前地址空间，因此 token 与用户地址共同参与
/// key；共享 futex 则以物理地址作为 key，让映射同一物理字的进程共享队列。
fn futex_queue_key(opt: FutexOpt, memory_token: usize, uaddr: usize, pa: usize) -> usize {
    if opt.contains(FutexOpt::FUTEX_PRIVATE_FLAG) {
        memory_token ^ uaddr
    } else {
        pa
    }
}

/// futex 系统调用入口。
///
/// `uaddr` 指向用户空间中 4 字节对齐的 futex word；`futex_op` 的低位
/// 是命令，高位包含私有 futex 与时钟选项。入口负责完成 ABI 校验、用户
/// 地址翻译、超时转换和第二个地址（requeue）的准备，具体等待/唤醒行为
/// 分派给下层辅助函数。
///
/// 当前实现支持 `WAIT`、`WAIT_BITSET`、`WAKE`、`WAKE_BITSET` 和
/// `REQUEUE`。其他命令明确返回 `ENOSYS`，而不是让无效用户请求触发内核
/// panic。相对超时使用单调时间；带 `FUTEX_CLOCK_REALTIME` 的 wait-bitset
/// 超时则按墙上时钟解释后转换为内核的绝对时间。
///
/// 参考：<https://man7.org/linux/man-pages/man2/futex.2.html>
pub fn sys_futex(
    uaddr: *mut i32, // 指向 futex word，固定 4 字节
    futex_op: u32,   // futex 操作（命令 + 标志位）
    val: i32,
    timeout: *const Timespec,
    uaddr2: *mut u32,
    val3: i32,
) -> SyscallRet {
    // Linux 的 FUTEX_CMD_MASK 只屏蔽两个已定义的标志位。不要屏蔽任意高位：
    // 否则会把一个非法的用户 ABI 请求变成另一个看似合法的命令。
    const FUTEX_CMD_MASK: u32 = !(0x80 | 0x100);
    let cmd = FutexCmd::try_from(futex_op & FUTEX_CMD_MASK).map_err(|_| SysErrNo::EINVAL)?;
    let opt = FutexOpt::from_bits_truncate(futex_op);
    if opt.contains(FutexOpt::FUTEX_CLOCK_REALTIME) && cmd != FutexCmd::WaitBitset {
        // Linux 接受 CLOCK_REALTIME 用于 FUTEX_WAIT_BITSET（以及本实现未覆盖的
        // 部分 PI 操作），但不接受用于普通 FUTEX_WAIT 或 wake/requeue 操作。
        return Err(SysErrNo::ENOSYS);
    }
    #[cfg(feature = "perf")]
    let active_guard = crate::utils::perf::FutexActiveGuard::new();
    // uaddr 指向 int，必须 4 字节对齐
    if uaddr.align_offset(4) != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    // debug!("[sys_futex]: strong_count = {}", Arc::strong_count(&task));
    let process = &task.process;
    let memory_set = process.memory_set_arc();

    // 安全地将用户 VA 转为 PA：先通过 copy_from_user 触发延迟页分配，
    // 确保页面已映射后再查询页表，避免在未分配页面上 panic
    let pa = translate_user_va_safe(&memory_set, VirtAddr::from(uaddr as usize))?;
    // 仅在 Requeue 操作时才需要翻译 uaddr2
    let queue_key2 = if cmd == FutexCmd::Requeue {
        let pa2 = translate_user_va_safe(&memory_set, VirtAddr::from(uaddr2 as usize))?;
        Some(futex_queue_key(
            opt,
            memory_set.token(),
            uaddr2 as usize,
            pa2,
        ))
    } else {
        None
    };

    // 处理超时参数：仅 WAIT / WAIT_BITSET 才会用到 timeout。
    let timeout_opt: Option<Timespec>;
    if timeout.is_null() || !(cmd == FutexCmd::Wait || cmd == FutexCmd::WaitBitset) {
        timeout_opt = None;
    } else {
        // 防御：timeout 是用户指针，不可能是 usize::MAX 这种特殊值
        if timeout as usize == usize::MAX {
            return Err(SysErrNo::EINVAL);
        }
        let mut real_timeout = Timespec::default();
        copy_from_user(&memory_set, timeout as usize, unsafe {
            core::slice::from_raw_parts_mut(
                &mut real_timeout as *mut Timespec as *mut _,
                core::mem::size_of::<Timespec>(),
            )
        })?;
        if opt.contains(FutexOpt::FUTEX_CLOCK_REALTIME) {
            // 此时 timeout 是相对 1970 的墙上时钟时间，而不是时间间隔。
            // 因此减去“开机时间相对 1970 的秒数”，得到内核使用的绝对时间。
            real_timeout.tv_sec -= crate::timer::NOW_TIME_STAMP;
        } else {
            // 传入的是相对时间间隔，而内核内部使用绝对单调时间：
            // 因此加上当前单调时间。
            real_timeout = real_timeout + get_time_spec();
        }
        timeout_opt = Some(real_timeout);
    }

    log::debug!(
        "[sys_futex] uaddr = {:x}, pa = {:#x}, cmd = {:?}, val = {}, opt={:?}",
        uaddr as usize,
        pa,
        cmd,
        val,
        opt
    );
    let queue_key = futex_queue_key(opt, memory_set.token(), uaddr as usize, pa);

    match cmd {
        FutexCmd::Wait | FutexCmd::WaitBitset => {
            #[cfg(feature = "perf")]
            drop(active_guard);
            let bitset = if cmd == FutexCmd::Wait {
                u32::MAX
            } else {
                val3 as u32
            };
            if bitset == 0 {
                return Err(SysErrNo::EINVAL);
            }
            futex_wait_bitset(
                queue_key,
                task,
                &memory_set,
                uaddr,
                val,
                bitset,
                timeout_opt,
            )
        }
        FutexCmd::Wake | FutexCmd::WakeBitset => {
            drop(task);
            let bitset = if cmd == FutexCmd::Wake {
                u32::MAX
            } else {
                val3 as u32
            };
            if bitset == 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(futex_wake_up_bitset(queue_key, val, bitset))
        }
        FutexCmd::Requeue => {
            drop(task);
            if let Some(queue_key2) = queue_key2 {
                // 注：FUTEX_REQUEUE 的 `max_requeue` 复用 timeout 参数指针值作为
                // i32。这与 Linux ABI 一致（val2 位置即 requeue 上限），依赖上层
                // 传入的实际计数而非指针；此处保留该行为以便与用户态 pthread
                // 实现兼容。
                return Ok(futex_requeue(queue_key, val, queue_key2, timeout as i32));
            } else {
                return Err(SysErrNo::EINVAL);
            }
        }

        _ => {
            // 用户的 ABI 请求绝不能让内核 panic：Linux 会拒绝未支持的 futex
            // 命令，本内核也只实现上面列出的几种操作。
            Err(SysErrNo::ENOSYS)
        }
    }
}

// ---------------------------------------------------------------------------
// Robust-futex helpers
// ---------------------------------------------------------------------------
/// 原子地（通过重试模拟 cmpxchg）把 futex word 标记为 `FUTEX_OWNER_DIED`，
/// 并唤醒等待者——但仅当该 futex 当前仍由 `pid` 持有时。
///
/// 重试循环：
///  1. 读取 `uval`；
///  2. 若 `(uval & TID_MASK) != pid`，说明并非我们持有，直接返回 `false`；
///  3. 写入 `(uval & WAITERS) | OWNER_DIED`（保留 WAITERS 位、清除 TID）；
///  4. 回读校验；若并发修改导致不一致，回到第 1 步。
///
/// 成功置位且原值带有 WAITERS 位时，会唤醒一个等待者。返回值表示退出
/// 线程是否确实持有了该 futex（并因此完成了更新）。若用户地址未映射
/// （例如进程已被 SIGSEGV 部分解除映射），则跳过并返回 `false`。
///
/// 参考：Linux `handle_futex_death()`（`kernel/futex/core.c`）。
fn handle_futex_death_entry(uaddr: usize, memory_set: &MemorySet, pid: usize) -> bool {
    debug!("[handle_futex_death_entry] uaddr={:#x}, pid={}", uaddr, pid);
    loop {
        // ---- 读取当前 futex word ----
        let uval: u32 = match try_copy_from_user_val(memory_set, uaddr as *const u32) {
            Some(v) => v,
            None => {
                debug!(
                    "[handle_futex_death_entry] uaddr {:#x} is unmapped, skip",
                    uaddr
                );
                return false;
            }
        };
        // 并非退出线程持有 → 无事可做。
        if uval as usize & FUTEX_TID_MASK as usize != pid {
            return false;
        }
        // 保留 WAITERS 位，设置 OWNER_DIED，清除 TID。
        let newval: u32 = (uval & FUTEX_WAITERS) | FUTEX_OWNER_DIED;
        // ---- 写回 ----
        let _ = copy_to_user_val(memory_set, uaddr as *mut u32, &newval);
        // ---- 回读校验（简易 cmpxchg）----
        let after: u32 = match try_copy_from_user_val(memory_set, uaddr as *const u32) {
            Some(v) => v,
            None => {
                debug!(
                    "[handle_futex_death_entry] uaddr {:#x} became unmapped, skip",
                    uaddr
                );
                return false;
            }
        };
        if after != newval {
            // 其他线程并发修改了该 word，重试。
            continue;
        }
        // ---- 若存在等待者，唤醒其中一个 ----
        if uval & FUTEX_WAITERS != 0 {
            futex_wake_robust_user_addr(memory_set, uaddr, 1);
        }

        return true;
    }
}

/// 线程退出时处理 robust futex 链表。
///
/// robust list 是用户空间维护的循环单链表。函数先处理
/// `list_op_pending`，再从 `head->list.next` 开始遍历，最多处理
/// [`ROBUST_LIST_LIMIT`] 个节点。对于仍由退出线程持有的 futex，
/// [`handle_futex_death_entry`] 会清除 owner TID、保留 WAITERS 标志、
/// 设置 `FUTEX_OWNER_DIED` 并唤醒一个等待者。
///
/// 链表和节点都来自用户空间，因此每次读取都允许失败；遇到未映射或
/// 损坏的地址时停止遍历而不让退出路径 panic。`futex_offset` 是从链表
/// 节点地址到 futex word 的有符号偏移，`pid` 是退出线程的 futex owner ID。
///
/// 参考：Linux `exit_robust_list()`（`kernel/futex/core.c`）。
pub fn handle_futex_when_exit(robust_list: &RobustListHead, memory_set: &MemorySet, pid: usize) {
    let head: usize = robust_list.list; // robust_list_head 的用户空间基址
    if head == 0 {
        debug!("[handle_futex_when_exit] robust_list.list is 0, nothing to do");
        return;
    }
    // ---- 从用户空间头部读取 futex_offset 与 list_op_pending ----
    // `struct robust_list_head` 布局（RV64 ABI）：
    //   +0  list.next          : usize
    //   +8  futex_offset       : isize   （entry → futex word 的有符号偏移）
    //   +16 list_op_pending    : usize
    //
    // 使用 try_copy_from_user_val：进程可能已被 SIGSEGV 杀死，其内存可能
    // 部分未映射（例如 glibc 探测 OOM 之后）。
    let futex_offset: isize = match try_copy_from_user_val(memory_set, (head + 8) as *const isize) {
        Some(v) => v,
        None => {
            debug!("[handle_futex_when_exit] head+8 unmapped, stopping");
            return;
        }
    };
    let list_op_pending: usize =
        match try_copy_from_user_val(memory_set, (head + 16) as *const usize) {
            Some(v) => v,
            None => {
                debug!("[handle_futex_when_exit] head+16 unmapped, stopping");
                return;
            }
        };
    debug!(
        "[handle_futex_when_exit] head={:#x}, futex_offset={}, list_op_pending={:#x}, pid={}",
        head, futex_offset, list_op_pending, pid,
    );
    // ---- 1. 先处理 *pending* 项（list_op_pending）----
    if list_op_pending != 0 {
        let futex_word_addr = (list_op_pending as isize).wrapping_add(futex_offset) as usize; // virtaddr
        debug!(
            "[handle_futex_when_exit] processing pending entry at {:#x}, futex_word={:#x}",
            list_op_pending, futex_word_addr,
        );
        handle_futex_death_entry(futex_word_addr, memory_set, pid);
        // 原子地清空 list_op_pending，让用户空间看到我们已经处理。
        let _ = copy_to_user_val(memory_set, (head + 16) as *mut usize, &0usize);
    }
    // ---- 2. 遍历循环 robust list ----
    // 第一个真实节点：head->list.next（list 位于偏移 0，即 *head）。
    let mut entry: usize = match try_copy_from_user_val(memory_set, head as *const usize) {
        Some(v) => v,
        None => {
            debug!("[handle_futex_when_exit] head unmapped, stopping");
            return;
        }
    };
    let mut limit: usize = ROBUST_LIST_LIMIT;
    while entry != head && limit > 0 {
        limit -= 1;
        // 读取当前节点的 next 指针。
        // 位 0 是 "list-op-pending" 标记，需要屏蔽掉。
        let raw_next: usize = match try_copy_from_user_val(memory_set, entry as *const usize) {
            Some(v) => v,
            None => {
                debug!(
                    "[handle_futex_when_exit] invalid entry {:#x}, stopping walk",
                    entry
                );
                break;
            }
        };
        let next: usize = raw_next & !1usize;
        // 计算 futex word 地址：entry + futex_offset
        let futex_word_addr = (entry as isize).wrapping_add(futex_offset) as usize;
        debug!(
            "[handle_futex_when_exit] entry={:#x}, futex_word={:#x}, next={:#x}",
            entry, futex_word_addr, next,
        );
        handle_futex_death_entry(futex_word_addr, memory_set, pid);
        entry = next;
    }
    if limit == 0 {
        error!(
            "[handle_futex_when_exit] ROBUST_LIST_LIMIT ({}) reached – list possibly corrupted",
            ROBUST_LIST_LIMIT,
        );
    }
    debug!("[handle_futex_when_exit] done");
}

/// futex wait 定时器回调：把超时的 waiter 从队列中摘下并标记超时。
///
/// `futex_key` 用于匹配本次等待，避免清理已经结束的等待（例如被正常
/// 唤醒或信号打断）的队列项。只有定时器真正获胜（key 仍匹配）时才会
/// 通过 [`timeout_futex_task`] 唤醒任务；若等待已被正常唤醒抢先处理，
/// 则保留成功唤醒的结果。
pub fn handle_timer(task: Arc<TaskControlBlock>, futex_key: usize) {
    bump_futex_queue_version();
    let mut waitq = FUTEX_QUEUE_BITMAP.lock();
    let inner = task.inner_lock();
    if inner.futex_key != futex_key {
        // 等待已结束（被正常唤醒/信号打断），无需处理。
        return;
    }
    let futex_pa = inner.futex_pa;
    drop(inner);
    // 从链表中取下这次 Wait
    let queue = waitq.get_mut(&futex_pa).expect("How could get_mut fail?");

    let idx = queue.iter().position(|x| x.futex_key == futex_key);
    if let Some(idx) = idx {
        queue.remove(idx);
        // 定时器只有仍在同一个阻塞代数时才获胜；并发的正常唤醒必须保留其
        // 成功唤醒的结果。
        timeout_futex_task(task, futex_key);
    }
}

/// 处理 `sigtimedwait` 超时：标记超时并通过 interrupt 唤醒 `block_on` 中的 `poll_fn`。
///
/// 与 futex wait 不同，这里不通过 futex 队列唤醒，而是直接调用
/// [`interrupt`](TaskControlBlock::interrupt)：`block_on` + `poll_fn`
/// 通过 `interrupt_waker` 注册了 waker，`interrupt()` 会触发它唤醒
/// `block_on` 循环。
pub fn handle_sigtimedwait_timer(task: Arc<TaskControlBlock>) {
    {
        let mut inner = task.inner_lock();
        inner.sigtimedwait_timedout = true;
    }
    // 使用 interrupt() 而非 wakeup_futex_task()：block_on + poll_fn 通过
    // interrupt_waker 注册了 waker，interrupt() 会触发它唤醒 block_on 循环。
    task.interrupt();
}
