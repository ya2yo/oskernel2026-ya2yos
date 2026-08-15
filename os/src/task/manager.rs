//!Implementation of [`TaskManager`]
use super::{current_task, ready_queue, TaskControlBlock, TaskStatus, INITPROC};
use crate::signal::deliver_blocked_itimer_signal;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use log::debug;
use spin::{Lazy, Mutex};

/// Complete one specific futex wait generation.
///
/// A task can enter a new wait before an older timeout or queued wakeup is
/// observed.  Only the waiter generation recorded in the queue may change
/// this task back to Ready; otherwise a stale entry could consume a
/// FUTEX_WAKE(1) and clear the new wait's bookkeeping.
pub fn wakeup_futex_task(task: Arc<TaskControlBlock>, futex_key: usize) -> bool {
    let should_enqueue = mark_futex_task_ready(&task, futex_key);
    if should_enqueue {
        ready_queue::add_task(&task);
    }
    should_enqueue
}

/// Mark a matching futex waiter ready without publishing it to the scheduler.
///
/// Futex wake paths call this while the futex operation lock is held, then add
/// the task to the ready queue after releasing all futex locks.  Keeping the
/// scheduler notification out of the futex critical section avoids a lock
/// chain through the ready queue.
pub fn mark_futex_task_ready(task: &TaskControlBlock, futex_key: usize) -> bool {
    let mut task_inner = task.inner_lock();
    // A timeout and a signal can race after the blocked task has already been
    // scheduled again.  Only the transition from Blocked owns a new enqueue;
    // changing Running or Zombie back to Ready creates a stale run-queue entry
    // that can execute after its kernel stack has been released.
    let should_enqueue =
        task_inner.task_status == TaskStatus::Blocked && task_inner.futex_key == futex_key;
    if should_enqueue {
        task_inner.task_status = TaskStatus::Ready;
        task_inner.futex_key = 0;
        task_inner.futex_pa = 0;
    }
    should_enqueue
}

/// Complete a matching futex wait because its deadline expired.
pub fn timeout_futex_task(task: Arc<TaskControlBlock>, futex_key: usize) -> bool {
    let should_enqueue = mark_futex_task_timeout(&task, futex_key);
    if should_enqueue {
        ready_queue::add_task(&task);
    }
    should_enqueue
}

/// Mark a matching futex waiter timed out without touching the scheduler.
pub fn mark_futex_task_timeout(task: &TaskControlBlock, futex_key: usize) -> bool {
    let mut task_inner = task.inner_lock();
    let should_enqueue =
        task_inner.task_status == TaskStatus::Blocked && task_inner.futex_key == futex_key;
    if should_enqueue {
        task_inner.futex_timedout = true;
        task_inner.task_status = TaskStatus::Ready;
        task_inner.futex_key = 0;
        task_inner.futex_pa = 0;
    }
    should_enqueue
}

/// Move one still-blocked futex wait to a different queue key.
pub fn requeue_futex_task(task: &TaskControlBlock, futex_key: usize, new_futex_pa: usize) -> bool {
    let mut task_inner = task.inner_lock();
    if task_inner.task_status != TaskStatus::Blocked || task_inner.futex_key != futex_key {
        return false;
    }
    task_inner.futex_pa = new_futex_pa;
    true
}

pub fn check_blocked_task_timers() {
    tid_to_task::for_each_task(|task| {
        // This scan is invoked by the globally claimed timer-maintenance
        // bucket.  One owner visits every blocked task, avoiding an otherwise
        // identical full task-table walk on every active Hart.
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
