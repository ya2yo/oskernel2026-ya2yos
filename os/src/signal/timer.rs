//! interval timer 触发的 signal 投递。
//!
//! 本模块管理 pselect 的 ITIMER_REAL 等待屏蔽状态，并在 timer 到期时向
//! 运行态或可中断阻塞态任务投递 `SIGALRM`。

use alloc::collections::BTreeSet;

use spin::{Lazy, Mutex};

use super::SigSet;
use crate::{
    task::{ready_queue, tid_to_task, TaskControlBlock, TaskStatus},
    timer::TimeVal,
};

static PSELECT_ITIMER_WAITERS: Lazy<Mutex<BTreeSet<usize>>> =
    Lazy::new(|| Mutex::new(BTreeSet::new()));

pub struct PselectItimerGuard {
    tid: usize,
}

impl Drop for PselectItimerGuard {
    fn drop(&mut self) {
        PSELECT_ITIMER_WAITERS.lock().remove(&self.tid);
    }
}

pub fn enter_pselect_itimer_wait(task: &TaskControlBlock) -> PselectItimerGuard {
    PSELECT_ITIMER_WAITERS.lock().insert(task.tid());
    PselectItimerGuard { tid: task.tid() }
}

fn is_pselect_itimer_waiter(task: &TaskControlBlock) -> bool {
    PSELECT_ITIMER_WAITERS.lock().contains(&task.tid())
}

fn add_itimer_signal(task: &TaskControlBlock) {
    let mut task_inner = task.inner_lock();
    task_inner.sig_pending |= SigSet::SIGALRM;
    let should_ready = task_inner.task_status == TaskStatus::Blocked;
    if should_ready {
        task_inner.task_status = TaskStatus::Ready;
    }
    drop(task_inner);

    if should_ready {
        if let Some(task) = tid_to_task::tid2task(task.tid()) {
            ready_queue::add_task(&task);
        }
    }
}

fn add_blocked_itimer_signal(task: &TaskControlBlock) {
    let task_inner = task.inner_lock();
    let should_wake = task_inner.task_status == TaskStatus::Blocked;
    drop(task_inner);

    if !should_wake {
        return;
    }

    // Future-based waits register an interrupt waker; ppoll/pause do not.
    // ITIMER_REAL must interrupt both kinds of blocked syscall, so waking the
    // optional Future is only an additional notification, not a prerequisite
    // for recording SIGALRM and making the task runnable.
    let _ = task.wake_interruptible();
    let mut task_inner = task.inner_lock();
    task_inner.sig_pending |= SigSet::SIGALRM;
    if task_inner.task_status == TaskStatus::Blocked {
        task_inner.task_status = TaskStatus::Ready;
        drop(task_inner);
        if let Some(task) = tid_to_task::tid2task(task.tid()) {
            ready_queue::add_task(&task);
        }
    }
}

/// Check a task's interval timer and deliver SIGALRM when it expires.
pub fn deliver_itimer_signal(task: &TaskControlBlock) {
    if is_pselect_itimer_waiter(task) {
        return;
    }

    let timer = {
        let task_inner = task.inner_lock();
        task_inner.timer.clone()
    };
    if timer.take_expired_signal(TimeVal::now()) {
        add_itimer_signal(task);
    }
}

/// Deliver expired ITIMER_REAL to a blocked task on its owner hart.
pub fn deliver_blocked_itimer_signal(task: &TaskControlBlock) {
    let timer = {
        let task_inner = task.inner_lock();
        task_inner.timer.clone()
    };
    if timer.take_expired_signal(TimeVal::now()) {
        add_blocked_itimer_signal(task);
    }
}
