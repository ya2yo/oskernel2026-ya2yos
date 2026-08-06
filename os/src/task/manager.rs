//!Implementation of [`TaskManager`]
use super::{current_task, ready_queue, TaskControlBlock, TaskStatus, INITPROC};
use crate::signal::deliver_blocked_itimer_signal;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use log::debug;
use spin::{Lazy, Mutex};

pub fn wakeup_futex_task(task: Arc<TaskControlBlock>) {
    let mut task_inner = task.inner_lock();
    // A timeout and a signal can race after the blocked task has already been
    // scheduled again.  Only the transition from Blocked owns a new enqueue;
    // changing Running or Zombie back to Ready creates a stale run-queue entry
    // that can execute after its kernel stack has been released.
    let should_enqueue = task_inner.task_status == TaskStatus::Blocked;
    if should_enqueue {
        task_inner.task_status = TaskStatus::Ready;
    }
    task_inner.futex_key = 0;
    task_inner.futex_pa = 0;
    drop(task_inner);
    if should_enqueue {
        ready_queue::add_task(&task);
    }
}

pub fn check_blocked_task_timers() {
    let hartid = crate::arch::cpu::hart_id();
    tid_to_task::for_each_task(|task| {
        // A blocked wait is owned by its thread's current scheduler placement.
        // This keeps timer delivery singular even when sibling threads sharing
        // an address space run on different harts.
        if task.scheduled_hart() != hartid {
            return;
        }
        // 这条补扫主要服务于阻塞在 accept/recv 等路径中的任务，避免它们在
        // 内核态调度循环中错过 ITIMER_REAL。具体到期判断和 SIGALRM 投递由
        // timer/signal 模块负责，任务管理器只负责遍历候选任务。
        let should_check = {
            let inner = task.inner_lock();
            inner.task_status == TaskStatus::Blocked
        };
        if should_check {
            deliver_blocked_itimer_signal(task);
        }
    });
}

pub mod tid_to_task {
    use alloc::vec::Vec;
    use log::debug;

    use super::{Arc, BTreeMap, Lazy, TaskControlBlock};
    use crate::sync::RemoteTlbMutex;
    static TID_TO_TASK: Lazy<RemoteTlbMutex<BTreeMap<usize, Arc<TaskControlBlock>>>> =
        Lazy::new(|| RemoteTlbMutex::new(BTreeMap::new()));

    pub fn tid2task(tid: usize) -> Option<Arc<TaskControlBlock>> {
        TID_TO_TASK.lock().get(&tid).map(Arc::clone)
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
        let mut all_tasks = Vec::new();
        // Keep Vec growth outside the task-table lock.  The snapshot callers
        // may allocate while filtering or deduplicating process targets.
        for_each_task(|task| all_tasks.push((task.tid(), Arc::clone(task))));
        all_tasks
    }

    /// Visit tasks without allocating a snapshot vector or holding the task
    /// table lock while entering task/signal code.
    pub fn for_each_task(mut f: impl FnMut(&Arc<TaskControlBlock>)) {
        let mut next_tid = 0;
        loop {
            let next = {
                let tasks = TID_TO_TASK.lock();
                tasks
                    .range(next_tid..)
                    .next()
                    .map(|(&tid, task)| (tid, Arc::clone(task)))
            };
            let Some((tid, task)) = next else {
                return;
            };

            f(&task);
            let Some(tid) = tid.checked_add(1) else {
                return;
            };
            next_tid = tid;
        }
    }
}
