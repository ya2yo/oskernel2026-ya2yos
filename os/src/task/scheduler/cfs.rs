use super::TaskControlBlock;
use crate::{arch::time::get_ticks, config::HART_NUM};
use alloc::{
    collections::BinaryHeap,
    sync::{Arc, Weak},
};
use core::{
    cmp::Ordering as CmpOrdering,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use log::warn;
use spin::{Lazy, Mutex};

const NICE_0_LOAD: u64 = 1024;
const NOT_RUNNING: u64 = u64::MAX;

// Linux CFS prio_to_weight table for nice values -20..19.
const NICE_WEIGHTS: [u32; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100, 4904,
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87,
    70, 56, 45, 36, 29, 23, 18, 15,
];

pub(crate) struct SchedEntity {
    vruntime: AtomicU64,
    exec_start: AtomicU64,
    on_rq: AtomicBool,
}

impl SchedEntity {
    pub const fn new() -> Self {
        Self {
            vruntime: AtomicU64::new(0),
            exec_start: AtomicU64::new(NOT_RUNNING),
            on_rq: AtomicBool::new(false),
        }
    }

    fn vruntime(&self) -> u64 {
        self.vruntime.load(Ordering::Acquire)
    }

    fn place_at(&self, min_vruntime: u64) -> u64 {
        let mut current = self.vruntime();
        while current < min_vruntime {
            match self.vruntime.compare_exchange_weak(
                current,
                min_vruntime,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return min_vruntime,
                Err(actual) => current = actual,
            }
        }
        current
    }

    fn mark_running(&self) {
        self.on_rq.store(false, Ordering::Release);
        self.exec_start.store(get_ticks() as u64, Ordering::Release);
    }

    fn account_runtime(&self, nice: i32) {
        let start = self.exec_start.swap(NOT_RUNNING, Ordering::AcqRel);
        if start == NOT_RUNNING {
            return;
        }
        let delta_exec = (get_ticks() as u64).wrapping_sub(start);
        let weight = NICE_WEIGHTS[(nice.clamp(-20, 19) + 20) as usize] as u64;
        let delta_vruntime = ((delta_exec as u128 * NICE_0_LOAD as u128) / weight as u128)
            .min(u64::MAX as u128) as u64;
        self.vruntime.fetch_add(delta_vruntime, Ordering::AcqRel);
    }

    fn try_mark_queued(&self) -> bool {
        self.on_rq
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

type QueueKey = (u64, usize);

struct CfsEntry {
    key: QueueKey,
    task: Weak<TaskControlBlock>,
}

impl PartialEq for CfsEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for CfsEntry {}

impl PartialOrd for CfsEntry {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for CfsEntry {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // BinaryHeap is a max-heap; reverse the key order to expose the
        // smallest (vruntime, tid) at peek/pop.
        other.key.cmp(&self.key)
    }
}

struct CfsHartRunQueue {
    tasks: BinaryHeap<CfsEntry>,
    min_vruntime: u64,
}

impl CfsHartRunQueue {
    fn new() -> Self {
        Self {
            tasks: BinaryHeap::new(),
            min_vruntime: 0,
        }
    }

    fn advance_min_vruntime(&mut self, current: u64) {
        let candidate = self
            .tasks
            .peek()
            .map(|leftmost| leftmost.key.0.min(current))
            .unwrap_or(current);
        self.min_vruntime = self.min_vruntime.max(candidate);
    }
}

static READY_QUEUES: Lazy<[Mutex<CfsHartRunQueue>; HART_NUM]> =
    Lazy::new(|| core::array::from_fn(|_| Mutex::new(CfsHartRunQueue::new())));

pub(super) fn add_task(task: &Arc<TaskControlBlock>) {
    let tid = task.tid();
    if !task.sched_entity.try_mark_queued() {
        warn!("add_task: task tid={} already in CFS queue, skipping", tid);
        return;
    }

    let hartid = task.process.home_hart();
    let mut hart_queue = READY_QUEUES[hartid].lock();
    let vruntime = task.sched_entity.place_at(hart_queue.min_vruntime);
    hart_queue.tasks.push(CfsEntry {
        key: (vruntime, tid),
        task: Arc::downgrade(task),
    });
}

pub(super) fn fetch_task(hartid: usize) -> Option<Arc<TaskControlBlock>> {
    let mut hart_queue = READY_QUEUES[hartid].lock();
    loop {
        let Some(entry) = hart_queue.tasks.pop() else {
            return None;
        };
        let Some(task) = entry.task.upgrade() else {
            warn!("fetch task got a dropped task");
            continue;
        };
        hart_queue.advance_min_vruntime(entry.key.0);
        return Some(task);
    }
}

pub(super) fn ready_procs_num() -> usize {
    READY_QUEUES
        .iter()
        .map(|queue| queue.lock().tasks.len())
        .sum()
}

pub(super) fn mark_running(task: &Arc<TaskControlBlock>) {
    task.sched_entity.mark_running();
}

pub(super) fn account_current(task: &Arc<TaskControlBlock>) {
    let nice = task.inner_lock().nice;
    task.sched_entity.account_runtime(nice);

    let hartid = task.process.home_hart();
    READY_QUEUES[hartid]
        .lock()
        .advance_min_vruntime(task.sched_entity.vruntime());
}
