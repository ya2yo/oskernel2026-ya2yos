use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    mm::{get_data, put_data, PhysAddr, VirtAddr},
    syscall::{FutexCmd, FutexOpt},
    task::RobustList,
    timer::{add_futex_timer, get_time_spec, Timespec},
    utils::{SysErrNo, SyscallRet},
};

use super::{block_current_and_run_next, current_task, wakeup_futex_task, TaskControlBlock};
use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
};
use log::{debug, error};
use spin::{Lazy, Mutex};

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
    log::debug!(
        "[futex_requeue],old_key={:?},max_wakeup={},new_key={:?},max_requeue={}",
        old_pa,
        max_wakeup,
        new_pa,
        max_requeue
    );
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
    // 在futex wait 的基础上，除了插入的队列不同，在队列项中多加了一个bitset，其他没有区别
    let mut waitq = FUTEX_QUEUE_BITMAP.lock();
    // 向key对应的等待队列中插入当前进程的弱指针。
    // 如果没有这个队列？那就新建一个
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
    // 释放锁……
    drop(task);
    drop(waitq);
    debug!("futex_wait_bitset sleeping...");
    block_current_and_run_next();
    debug!("futex_wait_bitset wake up!");
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    // woke by signal
    if !task_inner
        .sig_pending
        .difference(task_inner.sig_mask)
        .is_empty()
    {
        return Err(SysErrNo::EINTR);
    }
    debug!("futex_wait_bitset return!");
    Ok(0)
}

fn futex_wake_up_bitset(pa: usize, max_num: i32, bitset: u32) -> usize {
    log::debug!(
        "[sys_futex] futex wakeup thread,max_num={},key={:?}",
        max_num,
        pa
    );
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
    debug!("futex_wake_up_bitset: wake {} threads", num);
    num
}

static FUTEX_KEY_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn new_futex_key() -> usize {
    FUTEX_KEY_COUNTER.fetch_add(1, Ordering::Relaxed)
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
    debug!("Enter sys_futex");
    debug!("futex_op={}", futex_op);
    debug!("timeout={:#x}", timeout as usize);
    debug!("uaddr={:#x}", uaddr as usize);
    // let cmd = FutexCmd::from_bits(futex_op & 0x7f).unwrap();
    let cmd = FutexCmd::try_from(futex_op & 0x7f).expect("invalid futex op");
    debug!("futex cmd={:?}", cmd);
    let opt = FutexOpt::from_bits_truncate(futex_op);
    // 检查uaddr一定是4字节对齐（因为是int*）
    if uaddr.align_offset(4) != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    debug!("[sys_futex]: strong_count = {}", Arc::strong_count(&task));
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
        let mut real_timeout = get_data(token, timeout);
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
        debug!("real time out = {:?}", real_timeout);
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
            if get_data(token, uaddr) != val {
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

pub fn handle_futex_when_exit(_robust_list: &RobustList, _token: usize, _pid: usize) {
    // 不行，我们的实现是错误的
    // 宁可不运行这个函数
    return;
    // 以后再来改吧
}

pub fn handle_timer(task: Arc<TaskControlBlock>, futex_key: usize) {
    let mut waitq = FUTEX_QUEUE_BITMAP.lock();
    let inner = task.inner_lock();
    if inner.futex_key != futex_key {
        // do nothing
        return;
    }
    debug!(
        "handle_timer: task=(tid={},key={},pa={:#x})",
        task.tid(),
        inner.futex_key,
        inner.futex_pa
    );
    // 从链表中取下这次Wait
    let queue = waitq
        .get_mut(&inner.futex_pa)
        .expect("How could get_mut fail?");

    let idx = queue.iter().position(|x| x.futex_key == futex_key);
    if let Some(idx) = idx {
        queue.remove(idx);
        drop(inner);
        wakeup_futex_task(task);
    }
}
