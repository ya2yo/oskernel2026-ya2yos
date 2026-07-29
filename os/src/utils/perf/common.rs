//! Shared low-overhead primitives used by subsystem performance counters.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::time::get_clock_freq;

#[inline]
pub(crate) fn add(counter: &AtomicUsize, value: usize) {
    counter.fetch_add(value, Ordering::Relaxed);
}

#[inline]
pub(crate) fn update_max(counter: &AtomicUsize, value: usize) {
    let mut current = counter.load(Ordering::Relaxed);
    while value > current {
        match counter.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

#[inline]
pub(crate) fn record_duration(
    samples: &AtomicUsize,
    total: &AtomicUsize,
    maximum: &AtomicUsize,
    elapsed: usize,
) {
    add(samples, 1);
    add(total, elapsed);
    update_max(maximum, elapsed);
}

#[inline]
pub(crate) fn ticks_to_us(ticks: usize) -> usize {
    ticks
        .saturating_mul(1_000_000)
        .checked_div(get_clock_freq().max(1))
        .unwrap_or(usize::MAX)
}

pub(crate) fn emit_duration(
    label: &str,
    samples: &AtomicUsize,
    total: &AtomicUsize,
    maximum: &AtomicUsize,
) {
    println!(
        "{}(samples={} total_us={} max_us={})",
        label,
        samples.load(Ordering::Relaxed),
        ticks_to_us(total.load(Ordering::Relaxed)),
        ticks_to_us(maximum.load(Ordering::Relaxed)),
    );
}
