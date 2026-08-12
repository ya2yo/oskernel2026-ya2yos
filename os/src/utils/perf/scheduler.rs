//! Scheduler selection, wakeup placement, dispatch, and idle-loop counters.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::config::HART_NUM;

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
pub(crate) static SCHEDULER_SELECTIONS_BY_HART: [AtomicUsize; HART_NUM] =
    [const { AtomicUsize::new(0) }; HART_NUM];
pub(crate) static IDLE_LOOPS_BY_HART: [AtomicUsize; HART_NUM] =
    [const { AtomicUsize::new(0) }; HART_NUM];
pub(crate) static REMOTE_TLB_SHOOTDOWNS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_TARGET_HARTS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_ACKNOWLEDGEMENTS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_SHOOTDOWNS_BY_SOURCE: [AtomicUsize; 7] =
    [const { AtomicUsize::new(0) }; 7];
pub(crate) static REMOTE_TLB_LOCAL_ONLY_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_LOCAL_ONLY_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_LOCAL_ONLY_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_REMOTE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_REMOTE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_REMOTE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_MAILBOX_WAIT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_MAILBOX_WAIT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_MAILBOX_WAIT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_ACK_LATENCY_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_ACK_LATENCY_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static REMOTE_TLB_ACK_LATENCY_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static COW_EXCLUSIVE_UPGRADES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static COW_SHARED_FRAME_COPIES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_DISPATCH_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_DISPATCH_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SCHEDULER_DISPATCH_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TIMER_MAINTENANCE_CLAIMS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TIMER_MAINTENANCE_ACTIVE_SKIPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TIMER_MAINTENANCE_BUCKET_SKIPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TIMER_MAINTENANCE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TIMER_MAINTENANCE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TIMER_MAINTENANCE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
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

pub fn record_scheduler_selection(hartid: usize, self_selected: bool) {
    // fetch_add 返回递增后的序号，避免在每次调度选择后再次执行一次
    // 原子 load。BuildStorm 的 perf 快照中该计数超过十亿次，这个路径
    // 的额外原子操作会直接放大调度器测量开销。
    let selections = SCHEDULER_SELECTIONS.fetch_add(1, Ordering::Relaxed) + 1;
    if let Some(counter) = SCHEDULER_SELECTIONS_BY_HART.get(hartid) {
        add(counter, 1);
    }
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

#[inline]
pub fn record_timer_maintenance_claim() {
    add(&TIMER_MAINTENANCE_CLAIMS, 1);
}

#[inline]
pub fn record_timer_maintenance_active_skip() {
    add(&TIMER_MAINTENANCE_ACTIVE_SKIPS, 1);
}

#[inline]
pub fn record_timer_maintenance_bucket_skip() {
    add(&TIMER_MAINTENANCE_BUCKET_SKIPS, 1);
}

#[inline]
pub fn record_timer_maintenance_duration(elapsed: usize) {
    record_duration(
        &TIMER_MAINTENANCE_SAMPLES,
        &TIMER_MAINTENANCE_TICKS,
        &TIMER_MAINTENANCE_MAX_TICKS,
        elapsed,
    );
}

/// Record one active TCP receive poll closure, excluding time while
/// `poll_io` leaves the task blocked waiting for packet arrival.
#[inline]
pub fn record_idle_loop(hartid: usize) {
    add(&IDLE_LOOPS, 1);
    if let Some(counter) = IDLE_LOOPS_BY_HART.get(hartid) {
        add(counter, 1);
    }
}

/// Record a completed page-table update, its logical source, and its total
/// local-only or remote invalidation time.
#[inline]
pub fn record_remote_tlb_shootdown(source: usize, remote_targets: usize, elapsed: usize) {
    add(&REMOTE_TLB_SHOOTDOWNS, 1);
    add(&REMOTE_TLB_TARGET_HARTS, remote_targets);
    if let Some(counter) = REMOTE_TLB_SHOOTDOWNS_BY_SOURCE.get(source) {
        add(counter, 1);
    }
    if remote_targets == 0 {
        record_duration(
            &REMOTE_TLB_LOCAL_ONLY_SAMPLES,
            &REMOTE_TLB_LOCAL_ONLY_TICKS,
            &REMOTE_TLB_LOCAL_ONLY_MAX_TICKS,
            elapsed,
        );
    } else {
        record_duration(
            &REMOTE_TLB_REMOTE_SAMPLES,
            &REMOTE_TLB_REMOTE_TICKS,
            &REMOTE_TLB_REMOTE_MAX_TICKS,
            elapsed,
        );
    }
}

#[inline]
pub fn record_remote_tlb_mailbox_wait(elapsed: usize) {
    record_duration(
        &REMOTE_TLB_MAILBOX_WAIT_SAMPLES,
        &REMOTE_TLB_MAILBOX_WAIT_TICKS,
        &REMOTE_TLB_MAILBOX_WAIT_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_remote_tlb_acknowledgement(elapsed: usize) {
    add(&REMOTE_TLB_ACKNOWLEDGEMENTS, 1);
    record_duration(
        &REMOTE_TLB_ACK_LATENCY_SAMPLES,
        &REMOTE_TLB_ACK_LATENCY_TICKS,
        &REMOTE_TLB_ACK_LATENCY_MAX_TICKS,
        elapsed,
    );
}

/// Record how a present COW fault was resolved.
///
/// An exclusive source frame only needs a local PTE permission upgrade; a
/// shared frame receives a private replacement and follows remote TLB ACKs.
#[inline]
pub fn record_cow_fault_resolution(requires_copy: bool) {
    if requires_copy {
        add(&COW_SHARED_FRAME_COPIES, 1);
    } else {
        add(&COW_EXCLUSIVE_UPGRADES, 1);
    }
}

pub(crate) fn scheduler_hart_snapshot() -> ([usize; HART_NUM], [usize; HART_NUM]) {
    (
        core::array::from_fn(|hart| SCHEDULER_SELECTIONS_BY_HART[hart].load(Ordering::Relaxed)),
        core::array::from_fn(|hart| IDLE_LOOPS_BY_HART[hart].load(Ordering::Relaxed)),
    )
}

pub(crate) fn remote_tlb_shootdown_source_snapshot() -> [usize; 7] {
    core::array::from_fn(|source| REMOTE_TLB_SHOOTDOWNS_BY_SOURCE[source].load(Ordering::Relaxed))
}
