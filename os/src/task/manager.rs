//!Implementation of [`TaskManager`]
use super::{current_task, TaskControlBlock, TaskStatus, INITPROC};
use crate::signal::deliver_blocked_itimer_signal;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use hashbrown::HashSet;
use log::debug;
use spin::{Lazy, Mutex};

pub mod ready_queue {
    use log::warn;

    use super::*;
    /// 就绪队列，存储TCB的Arc
    static READY_QUEUE: Lazy<Mutex<VecDeque<Weak<TaskControlBlock>>>> =
        Lazy::new(|| Mutex::new(VecDeque::new()));

    // 一个HashMap，用于表示一个tid是否存于TASK_MANAGER中
    // static IN_QUEUE: Lazy<Mutex<HashSet<usize>>> = Lazy::new(|| Mutex::new(HashSet::new()));
    // 算了我们不要它了

    fn task_in_queue(
        queue_guard: &spin::MutexGuard<'_, VecDeque<Weak<TaskControlBlock>>>,
        task: &Arc<TaskControlBlock>,
    ) -> bool {
        for weak_tcb in queue_guard.iter() {
            if let Some(strong_tcb) = weak_tcb.upgrade() {
                if Arc::ptr_eq(&strong_tcb, task) {
                    return true;
                }
            } else {
                warn!("task_in_queue got a None task???");
            }
        }
        return false;
    }
    /// 向就绪队列中添加
    pub fn add_task(task: &Arc<TaskControlBlock>) {
        // debug!("task: {} add", task.tid());
        let mut queue = READY_QUEUE.lock();
        // debug!("{:?}", queue);
        if task_in_queue(&queue, task) {
            warn!(
                "add_task: task tid={} already in queue, skipping",
                task.tid()
            );
            return;
        }
        queue.push_back(Arc::downgrade(&task));
    }

    /// 从就绪队列中取出当前 hart 可以运行的任务。
    ///
    /// 在远程 TLB shootdown 完成前，进程固定到一个 hart；不属于当前
    /// hart 的任务保留在队列中等待其 owner，避免共享地址空间跨核并行。
    pub fn fetch_task(hartid: usize) -> Option<Arc<TaskControlBlock>> {
        let mut queue = READY_QUEUE.lock();
        let queued = queue.len();
        for _ in 0..queued {
            let Some(task) = queue.pop_front() else {
                break;
            };
            let Some(task_arc) = task.upgrade() else {
                warn!("fetch task got a dropped task");
                continue;
            };
            if task_arc.process.home_hart() == hartid {
                return Some(task_arc);
            }
            queue.push_back(Arc::downgrade(&task_arc));
        }
        None
    }
    /// 取就绪队列长度
    pub fn ready_procs_num() -> usize {
        READY_QUEUE.lock().len()
    }
}

pub fn wakeup_futex_task(task: Arc<TaskControlBlock>) {
    let mut task_inner = task.inner_lock();
    if task_inner.task_status == TaskStatus::Ready {
        // 任务已被信号唤醒并在就绪队列中，只需清理 futex 字段
        task_inner.futex_key = 0;
        task_inner.futex_pa = 0;
        drop(task_inner);
        return;
    }
    task_inner.task_status = TaskStatus::Ready;
    task_inner.futex_key = 0;
    task_inner.futex_pa = 0;
    drop(task_inner);
    ready_queue::add_task(&task);
}

pub fn check_blocked_task_timers() {
    for (_, task) in tid_to_task::get_all_tasks() {
        // 这条补扫主要服务于阻塞在 accept/recv 等路径中的任务，避免它们在
        // 内核态调度循环中错过 ITIMER_REAL。具体到期判断和 SIGALRM 投递由
        // timer/signal 模块负责，任务管理器只负责遍历候选任务。
        let should_check = {
            let inner = task.inner_lock();
            inner.task_status == TaskStatus::Blocked
        };
        if should_check {
            deliver_blocked_itimer_signal(&task);
        }
    }
}

pub mod tid_to_task {
    use log::debug;

    use super::{Arc, BTreeMap, Lazy, Mutex, TaskControlBlock, Vec};
    static TID_TO_TASK: Lazy<Mutex<BTreeMap<usize, Arc<TaskControlBlock>>>> =
        Lazy::new(|| Mutex::new(BTreeMap::new()));

    pub fn tid2task(tid: usize) -> Option<Arc<TaskControlBlock>> {
        TID_TO_TASK.lock().get(&tid).map(|x| x.clone())
    }
    /// 仅在clone时发生
    pub fn insert(tid: usize, task: &Arc<TaskControlBlock>) {
        TID_TO_TASK.lock().insert(tid, task.clone());
    }
    /// 仅在exit时发生
    pub fn remove(tid: usize) {
        // debug!("[tid_to_task]: remove {}!", tid);
        let ret = TID_TO_TASK.lock().remove(&tid);
        if ret.is_none() {
            panic!("fail to remove task {}! it does not exist!", tid);
        }
    }
    /// tid_to_task模块记录的task数量
    pub fn task_num() -> usize {
        TID_TO_TASK.lock().len()
    }

    /// 构造一个vector，包括所有的TCB和他们的tid
    pub fn get_all_tasks() -> Vec<(usize, Arc<TaskControlBlock>)> {
        TID_TO_TASK
            .lock()
            .iter()
            .map(|(&tid, task)| (tid, Arc::clone(task)))
            .collect()
    }
}
