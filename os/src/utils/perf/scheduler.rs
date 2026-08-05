//! Scheduler selection, wakeup placement, dispatch, and idle-loop counters.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::common::{add, record_duration};
use super::maybe_report;

pub(crate) static SCHEDULER_LOCAL_ENQUEUES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_REMOTE_ENQUEUES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_REMOTE_IDLE_NOTIFICATIONS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_REMOTE_IPI_SENT: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_REMOTE_IPI_FAILED: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_SELECTIONS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_SELF_SELECTIONS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static IDLE_LOOPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_DISPATCH_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_DISPATCH_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_DISPATCH_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub fn record_scheduler_enqueue(remote: bool, target_idle: bool, ipi_sent: bool) {
    if !remote {
        add(&SCHEDULER_LOCAL_ENQUEUES, 1);
        return;
    }

    add(&SCHEDULER_REMOTE_ENQUEUES, 1);
    if target_idle {
        add(&SCHEDULER_REMOTE_IDLE_NOTIFICATIONS, 1);
        if ipi_sent {
            add(&SCHEDULER_REMOTE_IPI_SENT, 1);
        } else {
            add(&SCHEDULER_REMOTE_IPI_FAILED, 1);
        }
    }
}

pub fn record_scheduler_selection(self_selected: bool) {
    // fetch_add 返回递增后的序号，避免在每次调度选择后再次执行一次
    // 原子 load。BuildStorm 的 perf 快照中该计数超过十亿次，这个路径
    // 的额外原子操作会直接放大调度器测量开销。
    let selections = SCHEDULER_SELECTIONS.fetch_add(1, Ordering::Relaxed) + 1;
    if self_selected {
        add(&SCHEDULER_SELF_SELECTIONS, 1);
    }
    if selections & 0x0fff == 0 {
        maybe_report();
    }
}

/// Record scheduler work between returning to the idle scheduler context and
/// selecting the next runnable task. The task's actual execution and context
/// switch are intentionally outside this interval.
#[inline]
pub fn record_scheduler_dispatch_duration(elapsed: usize) {
    record_duration(
        &SCHEDULER_DISPATCH_SAMPLES,
        &SCHEDULER_DISPATCH_TICKS,
        &SCHEDULER_DISPATCH_MAX_TICKS,
        elapsed,
    );
}

/// Record one active TCP receive poll closure, excluding time while
/// `poll_io` leaves the task blocked waiting for packet arrival.
#[inline]
pub fn record_idle_loop() {
    add(&IDLE_LOOPS, 1);
}
