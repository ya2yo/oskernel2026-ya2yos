//! Network-stack performance counters that are below the syscall boundary.

use core::sync::atomic::AtomicUsize;

use super::common::record_duration;

pub(crate) static TCP_RECV_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TCP_RECV_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TCP_RECV_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub fn record_tcp_recv_active_duration(elapsed: usize) {
    record_duration(
        &TCP_RECV_ACTIVE_SAMPLES,
        &TCP_RECV_ACTIVE_TICKS,
        &TCP_RECV_ACTIVE_MAX_TICKS,
        elapsed,
    );
}
