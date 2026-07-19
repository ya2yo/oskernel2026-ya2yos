use super::TaskControlBlock;
use crate::task::TaskStatus;
use alloc::{
    collections::{BTreeSet, VecDeque},
    sync::{Arc, Weak},
};
use log::warn;
use spin::{Lazy, Mutex};

struct RrRunQueue {
    tasks: VecDeque<(usize, Weak<TaskControlBlock>)>,
    queued_tids: BTreeSet<usize>,
}

impl RrRunQueue {
    fn new() -> Self {
        Self {
            tasks: VecDeque::new(),
            queued_tids: BTreeSet::new(),
        }
    }
}

/// 原有的全局先进先出 RR 就绪队列；集合只用于避免线性去重开销。
static READY_QUEUE: Lazy<Mutex<RrRunQueue>> = Lazy::new(|| Mutex::new(RrRunQueue::new()));

pub(super) fn add_task(task: &Arc<TaskControlBlock>) {
    let mut queue = READY_QUEUE.lock();
    if !queue.queued_tids.insert(task.tid()) {
        warn!(
            "add_task: task tid={} already in RR queue, skipping",
            task.tid()
        );
        return;
    }
    queue.tasks.push_back((task.tid(), Arc::downgrade(task)));
}

pub(super) fn fetch_task(hartid: usize) -> Option<Arc<TaskControlBlock>> {
    let mut queue = READY_QUEUE.lock();
    let queued = queue.tasks.len();
    for _ in 0..queued {
        let Some((tid, task)) = queue.tasks.pop_front() else {
            break;
        };
        let Some(task) = task.upgrade() else {
            queue.queued_tids.remove(&tid);
            warn!("fetch task got a dropped task");
            continue;
        };
        if task.process.home_hart() == hartid {
            queue.queued_tids.remove(&tid);
            let status = task.inner_lock().task_status;
            if status != TaskStatus::Ready {
                warn!(
                    "fetch_task: discard stale RR entry tid={}, status={:?}",
                    tid, status
                );
                continue;
            }
            return Some(task);
        }
        queue.tasks.push_back((tid, Arc::downgrade(&task)));
    }
    None
}

pub(super) fn ready_procs_num() -> usize {
    READY_QUEUE.lock().tasks.len()
}

pub(super) fn mark_running(_task: &Arc<TaskControlBlock>) {}

pub(super) fn account_current(_task: &Arc<TaskControlBlock>) {}
