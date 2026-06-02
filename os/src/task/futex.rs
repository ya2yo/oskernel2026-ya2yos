use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    arch::page_table::PageTable,
    mm::{PhysAddr, VirtAddr, copy_from_user, get_data, put_data, try_get_data},
    syscall::{FutexCmd, FutexOpt},
    task::{RobustListHead, tid_to_task},
    timer::{Timespec, add_futex_timer, get_time_spec},
    utils::{SysErrNo, SyscallRet},
};

use super::{block_current_and_run_next, current_task, wakeup_futex_task, TaskControlBlock};
use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
};
use log::{debug, error};
use spin::{Lazy, Mutex};

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

struct FutexWaiter {
    pub task: Weak<TaskControlBlock>,
    pub bitset: u32,
    pub futex_key: usize,
}

type BitsetWaitQueue = VecDeque<FutexWaiter>; // 这个u32是sys_wait_bitset的那个bitset

// bitset用的队列的映射
static FUTEX_QUEUE_BITMAP: Lazy<Mutex<BTreeMap<usize, BitsetWaitQueue>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));
// 唤醒在 pa 等待的线程
pub fn futex_wake_up(pa: usize, max_num: i32) -> usize {
    // 重定向需求
    return futex_wake_up_bitset(pa, max_num, u32::MAX);
}

fn futex_requeue(old_pa: usize, max_wakeup: i32, new_pa: usize, max_requeue: i32) -> usize {
    // log::debug!(
    //     "[futex_requeue],old_key={:?},max_wakeup={},new_key={:?},max_requeue={}",
    //     old_pa,
    //     max_wakeup,
    //     new_pa,
    //     max_requeue
    // );
    let mut futex_queue = FUTEX_QUEUE_BITMAP.lock();
    let mut num = 0;
    let mut num2 = 0;
    let mut tmp = VecDeque::new();
    if let Some(queue) = futex_queue.get_mut(&old_pa) {
        while let Some(waiter) = queue.pop_front() {
            if let Some(task) = waiter.task.upgrade() {
                if num < max_wakeup {
                    wakeup_futex_task(task);
                    num += 1;
                } else if num2 < max_requeue {
                    tmp.push_back(waiter);
                    num2 += 1;
                }
            }
        }
    }
    if !tmp.is_empty() {
        futex_queue
            .entry(new_pa)
            .or_insert_with(VecDeque::new)
            .extend(tmp);
    }
    num as usize
}

// 含bitset的futex
fn futex_wait_bitset(
    pa: usize,
    task: Arc<TaskControlBlock>,
    bitset: u32,
    timeout: Option<Timespec>,
) -> SyscallRet {
    debug!("wait bitset = {:b}", bitset);
    // 清除上次的定时器超时标记
    task.inner_lock().futex_timedout = false;

    let mut waitq = FUTEX_QUEUE_BITMAP.lock();
    let futex_key = new_futex_key();
    let waiter = FutexWaiter {
        task: Arc::downgrade(&task),
        bitset,
        futex_key,
    };
    let mut inner = task.inner_lock();
    inner.futex_pa = pa;
    inner.futex_key = futex_key;
    drop(inner);

    if let Some(timeout) = timeout {
        add_futex_timer(timeout, &task, futex_key);
    }

    if let Some(queue) = waitq.get_mut(&pa) {
        queue.push_back(waiter);
    } else {
        waitq.insert(pa, {
            let mut queue = VecDeque::new();
            queue.push_back(waiter);
            queue
        });
    }
    drop(task);
    drop(waitq);
    debug!("futex_wait_bitset sleeping...");
    block_current_and_run_next();
    debug!("futex_wait_bitset wake up!");
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
    // log::debug!(
    //     "[sys_futex] futex wakeup thread,max_num={},key={:?}",
    //     max_num,
    //     pa
    // );
    let mut futex_queue = FUTEX_QUEUE_BITMAP.lock();
    let mut num: usize = 0;
    if let Some(queue) = futex_queue.get_mut(&pa) {
        let queue_len = queue.len();
        // 我们会遍历这个deque，最多len次
        let mut cnt: usize = 0;
        while cnt < queue_len && num < (max_num as usize) {
            cnt += 1;
            if let Some(waiter) = queue.pop_front() {
                if let Some(task) = waiter.task.upgrade() {
                    // 需要检查：是不是确实相交不为0

                    if bitset & waiter.bitset != 0 {
                        wakeup_futex_task(task);
                        num += 1;
                    } else {
                        // 我还得给它还回去
                        // TODO: 这里也许可以做性能优化？
                        queue.push_back(waiter);
                    }
                } else {
                    panic!("Fail to upgrate weak_task!");
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

/// 参考 https://man7.org/linux/man-pages/man2/futex.2.html
pub fn sys_futex(
    uaddr: *mut i32, // point to the futex word, always four-bytes
    futex_op: u32,   // operation on futex
    val: i32,
    timeout: *const Timespec,
    uaddr2: *mut u32,
    _val3: i32,
) -> SyscallRet {
    let cmd = FutexCmd::try_from(futex_op & 0x7f).map_err(|_| SysErrNo::EINVAL)?;
    let opt = FutexOpt::from_bits_truncate(futex_op);
    // 检查uaddr一定是4字节对齐（因为是int*）
    if uaddr.align_offset(4) != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    // debug!("[sys_futex]: strong_count = {}", Arc::strong_count(&task));
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let task_inner = task.inner_lock();
    let token = memory_set.token();
    let pa = memory_set
        .translate_va(VirtAddr::from(uaddr as usize))
        .unwrap()
        .0;

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
        copy_from_user(&memory_set, timeout as usize, unsafe{
            core::slice::from_raw_parts_mut(&mut real_timeout as *mut Timespec as *mut _, 
            core::mem::size_of::<Timespec>())
        })?;
        if opt.contains(FutexOpt::FUTEX_CLOCK_REALTIME) {
            // 此时的timeout是相对于1970年的时间，而非时间间隔
            // 因此，我们减去“开机时间-1970”
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
        "[sys_futex] uaddr = {:x}, pa = {:?}, cmd = {:?}, val = {},opt={:?}",
        uaddr as usize,
        pa,
        cmd,
        val,
        opt
    );

    let pa2 = memory_set.translate_va(VirtAddr::from(uaddr2 as usize));
    drop(memory_set);
    drop(task_inner);
    drop(process);
    match cmd {
        FutexCmd::Wait | FutexCmd::WaitBitset => {
            // 理论上，既然会进入到这里，那么，用户态程序应该是获取锁失败了
            // 在这里做出检查：取出uaddr的值，看看到底是不是!=val
            if try_get_data(token, uaddr).ok_or(SysErrNo::EFAULT)? != val {
                return Err(SysErrNo::EAGAIN);
            }
            let bitset = if cmd == FutexCmd::Wait {
                u32::MAX
            } else {
                _val3 as u32
            };
            futex_wait_bitset(pa, task, bitset, timeout_opt) // 这合适吗？
        }
        FutexCmd::Wake | FutexCmd::WakeBitset => {
            drop(task);
            let bitset = if cmd == FutexCmd::Wake {
                u32::MAX
            } else {
                _val3 as u32
            };
            Ok(futex_wake_up_bitset(pa, val, bitset)) // 这合适吗？
        }
        FutexCmd::Requeue => {
            drop(task);
            if let Some(pa2) = pa2 {
                return Ok(futex_requeue(pa, val, pa2.0, timeout as i32));
            } else {
                return Err(SysErrNo::EINVAL);
            }
        }

        _ => {
            println!("Unimplemented: futex_op = {}", futex_op);
            unimplemented!();
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
fn handle_futex_death_entry(uaddr: usize, token: usize, pid: usize) -> bool {
    debug!("[handle_futex_death_entry] uaddr={:#x}, token={}, pid={}", uaddr, token, pid);
    loop {
        // ---- read current futex word ----
        let uval: u32 = match try_get_data(token, uaddr as *const u32) {
            Some(v) => v,
            None => {
                debug!("[handle_futex_death_entry] uaddr {:#x} is unmapped, skip", uaddr);
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
        put_data(token, uaddr as *mut u32, newval);
        // ---- verify (poor-man's cmpxchg) ----
        let after: u32 = match try_get_data(token, uaddr as *const u32) {
            Some(v) => v,
            None => {
                debug!("[handle_futex_death_entry] uaddr {:#x} became unmapped, skip", uaddr);
                return false;
            }
        };
        if after != newval {
            // Someone else modified the word concurrently – retry.
            continue;
        }
        // ---- wake one waiter if there were any ----
        if uval & FUTEX_WAITERS != 0 {
            let page_table = PageTable::from_token(token);
            if let Some(pa) = page_table.translate_va(VirtAddr::from(uaddr)) {
                futex_wake_up(pa.0, 1);
            }
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
pub fn handle_futex_when_exit(robust_list: &RobustListHead, token: usize, pid: usize) {
    let head: usize = robust_list.list;// User-space base address of robust_list_head
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
    // Use try_get_data: the process may have been killed by SIGSEGV and
    // its memory may be partially unmapped (e.g. after glibc probing OOM).
    let futex_offset: isize = match try_get_data(token, (head + 8) as *const isize) {
        Some(v) => v,
        None => {
            debug!("[handle_futex_when_exit] head+8 unmapped, stopping");
            return;
        }
    };
    let list_op_pending: usize = match try_get_data(token, (head + 16) as *const usize) {
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
        let futex_word_addr = (list_op_pending as isize).wrapping_add(futex_offset) as usize;// virtaddr
        debug!(
            "[handle_futex_when_exit] processing pending entry at {:#x}, futex_word={:#x}",
            list_op_pending, futex_word_addr,
        );
        handle_futex_death_entry(futex_word_addr, token, pid);
        // Atomically clear list_op_pending so userspace sees we handled it.
        put_data(token, (head + 16) as *mut usize, 0usize);
    }
    // ---- 2. Walk the circular robust list ----
    // First real entry: head->list.next (= *head because list is at offset 0).
    let mut entry: usize = match try_get_data(token, head as *const usize) {
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
        let raw_next: usize = match try_get_data(token, entry as *const usize) {
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
        handle_futex_death_entry(futex_word_addr, token, pid);
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
    let mut waitq = FUTEX_QUEUE_BITMAP.lock();
    let inner = task.inner_lock();
    if inner.futex_key != futex_key {
        // do nothing
        return;
    }
    let futex_pa = inner.futex_pa;
    drop(inner);
    // 从链表中取下这次Wait
    let queue = waitq
        .get_mut(&futex_pa)
        .expect("How could get_mut fail?");

    let idx = queue.iter().position(|x| x.futex_key == futex_key);
    if let Some(idx) = idx {
        queue.remove(idx);
        // 标记超时，确保 futex_wait_bitset 返回 ETIMEDOUT 而非 Ok(0)
        {
            let mut inner = task.inner_lock();
            inner.futex_timedout = true;
        }
        wakeup_futex_task(task);
    }
}
