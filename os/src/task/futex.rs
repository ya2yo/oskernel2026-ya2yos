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
/// Bit 31: there are waiters sleeping on this futex
const FUTEX_WAITERS: u32 = 0x80000000;
/// Bit 30: the original owner died while holding this futex
const FUTEX_OWNER_DIED: u32 = 0x40000000;
/// Bits 0..29: thread id
const FUTEX_TID_MASK: u32 = 0x3fffffff;
/// Upper bound on the number of robust-list entries we walk before giving up
const ROBUST_LIST_LIMIT: usize = 2048;

// -------------------------type defs--------------------------------

pub struct FutexWaiter {
    pub task: Weak<TaskControlBlock>,
    pub bitset: u32,
    pub futex_key: usize,
}

type BitsetWaitQueue = VecDeque<FutexWaiter>; // 这个u32是sys_wait_bitset的那个bitset

// bitset用的队列的映射
pub static FUTEX_QUEUE_BITMAP: Lazy<RemoteTlbMutex<BTreeMap<usize, BitsetWaitQueue>>> =
    Lazy::new(|| RemoteTlbMutex::new(BTreeMap::new()));
static FUTEX_QUEUE_VERSION: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn bump_futex_queue_version() {
    FUTEX_QUEUE_VERSION.fetch_add(1, Ordering::AcqRel);
}
// 唤醒在 pa 等待的线程
pub fn futex_wake_up(pa: usize, max_num: i32) -> usize {
    // 重定向需求
    return futex_wake_up_bitset(pa, max_num, u32::MAX);
}

fn futex_wake_robust_user_addr(memory_set: &MemorySet, uaddr: usize, max_num: i32) -> usize {
    let mut woken = 0;
    if let Ok(pa) = translate_user_va_safe(memory_set, VirtAddr::from(uaddr)) {
        woken += futex_wake_up_bitset(pa, max_num, u32::MAX);
    }
    woken += futex_wake_up_bitset(memory_set.token() ^ uaddr, max_num, u32::MAX);
    woken
}

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

fn new_futex_key() -> usize {
    // +1 确保 key 从 1 开始，0 表示"无/已清理的 Waiter"
    FUTEX_KEY_COUNTER.fetch_add(1, Ordering::Relaxed) + 1
}

fn futex_queue_key(opt: FutexOpt, memory_token: usize, uaddr: usize, pa: usize) -> usize {
    if opt.contains(FutexOpt::FUTEX_PRIVATE_FLAG) {
        memory_token ^ uaddr
    } else {
        pa
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/futex.2.html
pub fn sys_futex(
    uaddr: *mut i32, // point to the futex word, always four-bytes
    futex_op: u32,   // operation on futex
    val: i32,
    timeout: *const Timespec,
    uaddr2: *mut u32,
    val3: i32,
) -> SyscallRet {
    // Linux's FUTEX_CMD_MASK removes only the two defined flag bits.  Do not
    // mask arbitrary high bits: doing so turns an invalid user ABI request
    // into an unrelated valid command.
    const FUTEX_CMD_MASK: u32 = !(0x80 | 0x100);
    let cmd = FutexCmd::try_from(futex_op & FUTEX_CMD_MASK).map_err(|_| SysErrNo::EINVAL)?;
    let opt = FutexOpt::from_bits_truncate(futex_op);
    if opt.contains(FutexOpt::FUTEX_CLOCK_REALTIME) && cmd != FutexCmd::WaitBitset {
        // Linux accepts CLOCK_REALTIME for FUTEX_WAIT_BITSET (and selected PI
        // operations not implemented here), but not for plain FUTEX_WAIT or
        // wake/requeue operations.
        return Err(SysErrNo::ENOSYS);
    }
    #[cfg(feature = "perf")]
    let active_guard = crate::utils::perf::FutexActiveGuard::new();
    // 检查uaddr一定是4字节对齐（因为是int*）
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

    // 处理时间问题
    // 仅有在Wait时，才需考虑timeout
    let timeout_opt: Option<Timespec>;
    if timeout.is_null() || !(cmd == FutexCmd::Wait || cmd == FutexCmd::WaitBitset) {
        timeout_opt = None;
    } else {
        // 奇奇怪怪，timeout怎么能是-1呢？
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
            // 此时的timeout是相对于1970年的时间，而非时间间隔
            // 因此，我们减去"开机时间-1970"
            real_timeout.tv_sec -= crate::timer::NOW_TIME_STAMP; // 开机时间相对于1970的秒数
                                                                 // 用一个常数定义这个秒数还是有点太奇怪了，应该想办法改掉
        } else {
            // 传入的是相对时间，但是后面想要的是绝对单调时间（CPU自己计算的时间）
            // 因此，我们加上CPU的现在时间
            real_timeout = real_timeout + get_time_spec();
            // real_timeout.tv_sec += crate::timer::NOW_TIME_STAMP;
        }
        // debug!("real time out = {:?}", real_timeout);
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
                return Ok(futex_requeue(queue_key, val, queue_key2, timeout as i32));
            } else {
                return Err(SysErrNo::EINVAL);
            }
        }

        _ => {
            // A userspace ABI request must never turn into a kernel panic.
            // Linux rejects unsupported futex commands; this kernel implements
            // only the operations handled above.
            Err(SysErrNo::ENOSYS)
        }
    }
}

// ---------------------------------------------------------------------------
// Robust-futex helpers
// ---------------------------------------------------------------------------
/// Reference: linux7.0
/// Atomically mark a futex word as `FUTEX_OWNER_DIED` and wake waiters,
/// but *only when* the futex is currently owned by `pid`.
///
/// Returns `true` if we owned the futex (and therefore performed the update),
/// `false` otherwise.
///
/// The update uses a retry loop to emulate a hardware cmpxchg:
///  1. read  `uval`
///  2. if `(uval & TID_MASK) != pid` → not ours, return false
///  3. write `(uval & WAITERS) | OWNER_DIED`
///  4. re-read to verify → if someone raced with us, go to 1.
fn handle_futex_death_entry(uaddr: usize, memory_set: &MemorySet, pid: usize) -> bool {
    debug!("[handle_futex_death_entry] uaddr={:#x}, pid={}", uaddr, pid);
    loop {
        // ---- read current futex word ----
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
        // Not owned by the exiting task → nothing to do.
        if uval as usize & FUTEX_TID_MASK as usize != pid {
            return false;
        }
        // Preserve the WAITERS bit, set OWNER_DIED, clear TID.
        let newval: u32 = (uval & FUTEX_WAITERS) | FUTEX_OWNER_DIED;
        // ---- write back ----
        let _ = copy_to_user_val(memory_set, uaddr as *mut u32, &newval);
        // ---- verify (poor-man's cmpxchg) ----
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
            // Someone else modified the word concurrently – retry.
            continue;
        }
        // ---- wake one waiter if there were any ----
        if uval & FUTEX_WAITERS != 0 {
            futex_wake_robust_user_addr(memory_set, uaddr, 1);
        }

        return true;
    }
}

/// Process the robust futex list when a thread exits.
///
/// Reference:
///   Linux `exit_robust_list()` in `kernel/futex/core.c`
///   https://www.kernel.org/doc/html/latest/locking/robust-futexes.html
///
/// Walks the circular singly-linked robust list, marks every futex we still
/// own with `FUTEX_OWNER_DIED`, and wakes up one waiter per futex.
///
/// # Arguments
/// * `robust_list` – kernel-side copy of `RobustListHead`; its `list` field is
///   the user-space VA of the list head.
/// * `token`       – page-table token of the exiting process.
/// * `pid`         – TID of the exiting thread (used as futex owner id).
pub fn handle_futex_when_exit(robust_list: &RobustListHead, memory_set: &MemorySet, pid: usize) {
    let head: usize = robust_list.list; // User-space base address of robust_list_head
    if head == 0 {
        debug!("[handle_futex_when_exit] robust_list.list is 0, nothing to do");
        return;
    }
    // ---- Read futex_offset & list_op_pending from the user-space head ----
    // Layout of `struct robust_list_head` (RV64 ABI):
    //   +0  list.next          : usize
    //   +8  futex_offset       : isize   (signed offset from entry → futex word)
    //   +16 list_op_pending    : usize
    //
    // Use try_copy_from_user_val: the process may have been killed by SIGSEGV and
    // its memory may be partially unmapped (e.g. after glibc probing OOM).
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
    // ---- 1. Handle the *pending* entry (list_op_pending) first ----
    if list_op_pending != 0 {
        let futex_word_addr = (list_op_pending as isize).wrapping_add(futex_offset) as usize; // virtaddr
        debug!(
            "[handle_futex_when_exit] processing pending entry at {:#x}, futex_word={:#x}",
            list_op_pending, futex_word_addr,
        );
        handle_futex_death_entry(futex_word_addr, memory_set, pid);
        // Atomically clear list_op_pending so userspace sees we handled it.
        let _ = copy_to_user_val(memory_set, (head + 16) as *mut usize, &0usize);
    }
    // ---- 2. Walk the circular robust list ----
    // First real entry: head->list.next (= *head because list is at offset 0).
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
        // Read the next pointer from the current entry.
        // Bit 0 is the "list-op-pending" marker – mask it off.
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
        // Compute futex-word address: entry + futex_offset
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

pub fn handle_timer(task: Arc<TaskControlBlock>, futex_key: usize) {
    bump_futex_queue_version();
    let mut waitq = FUTEX_QUEUE_BITMAP.lock();
    let inner = task.inner_lock();
    if inner.futex_key != futex_key {
        // do nothing
        return;
    }
    let futex_pa = inner.futex_pa;
    drop(inner);
    // 从链表中取下这次Wait
    let queue = waitq.get_mut(&futex_pa).expect("How could get_mut fail?");

    let idx = queue.iter().position(|x| x.futex_key == futex_key);
    if let Some(idx) = idx {
        queue.remove(idx);
        // The timer wins only if this is still the same blocked generation.
        // A concurrent normal wake must retain its successful result.
        timeout_futex_task(task, futex_key);
    }
}

/// 处理 sigtimedwait 超时：标记超时并通过 interrupt 唤醒 block_on 中的 poll_fn
pub fn handle_sigtimedwait_timer(task: Arc<TaskControlBlock>) {
    {
        let mut inner = task.inner_lock();
        inner.sigtimedwait_timedout = true;
    }
    // 使用 interrupt() 而非 wakeup_futex_task()：block_on + poll_fn 通过
    // interrupt_waker 注册了 waker，interrupt() 会触发它唤醒 block_on 循环。
    task.interrupt();
}
