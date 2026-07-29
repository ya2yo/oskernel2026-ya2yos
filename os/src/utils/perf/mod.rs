//! Lightweight aggregate counters used to diagnose long-running workloads.
//!
//! Subsystem modules own their counters and recording APIs.  This facade keeps
//! existing `crate::utils::perf::*` call sites stable and serializes reporting.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::timer::get_time_ms;

mod common;
mod fs;
mod net;
mod report;
mod scheduler;
mod syscall;
mod task;

pub use fs::*;
pub use net::*;
pub use scheduler::*;
pub use syscall::*;
pub use task::*;

const REPORT_INTERVAL_MS: usize = 30_000;

static LAST_REPORT_MS: AtomicUsize = AtomicUsize::new(0);

/// Emit a cumulative snapshot at most once per interval.
pub fn maybe_report() {
    let now = get_time_ms();
    let previous = LAST_REPORT_MS.load(Ordering::Relaxed);
    if previous != 0 && now.saturating_sub(previous) < REPORT_INTERVAL_MS {
        return;
    }
    if LAST_REPORT_MS
        .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    report::emit_report(now);
}

/// Emit a report even when a short-lived test has not crossed the periodic
/// sampling interval.  The compare-exchange also prevents two harts from
/// printing the same snapshot when they shut down together.
pub fn report_now() {
    let now = get_time_ms();
    let previous = LAST_REPORT_MS.load(Ordering::Relaxed);
    if previous == now
        || LAST_REPORT_MS
            .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    report::emit_report(now);
}
