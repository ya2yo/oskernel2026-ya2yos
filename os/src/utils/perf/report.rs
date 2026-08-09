//! Periodic aggregate performance report formatting.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::fs::FILE_PAGE_CACHE;

use super::common::{emit_duration, ticks_to_us};
use super::fs::*;
use super::net::*;
use super::scheduler::*;
use super::syscall::*;
use super::task::*;

struct CounterDelta {
    last: AtomicUsize,
}

impl CounterDelta {
    const fn new() -> Self {
        Self {
            last: AtomicUsize::new(0),
        }
    }

    fn take(&self, counter: &AtomicUsize) -> usize {
        let current = counter.load(Ordering::Relaxed);
        let previous = self.last.swap(current, Ordering::Relaxed);
        current.saturating_sub(previous)
    }

    fn take_value(&self, current: usize) -> usize {
        let previous = self.last.swap(current, Ordering::Relaxed);
        current.saturating_sub(previous)
    }
}

struct DurationDelta {
    samples: AtomicUsize,
    ticks: AtomicUsize,
}

impl DurationDelta {
    const fn new() -> Self {
        Self {
            samples: AtomicUsize::new(0),
            ticks: AtomicUsize::new(0),
        }
    }
}

struct PhaseDelta {
    samples: AtomicUsize,
    ticks: AtomicUsize,
}

struct ResourceLockClassDelta {
    acquires: CounterDelta,
    contended: CounterDelta,
    queued: CounterDelta,
    wait_ticks: CounterDelta,
    hold_ticks: CounterDelta,
}

impl ResourceLockClassDelta {
    const fn new() -> Self {
        Self {
            acquires: CounterDelta::new(),
            contended: CounterDelta::new(),
            queued: CounterDelta::new(),
            wait_ticks: CounterDelta::new(),
            hold_ticks: CounterDelta::new(),
        }
    }
}

struct ResourceLockIoDelta {
    submits: CounterDelta,
    bytes: CounterDelta,
    queue_wait_samples: CounterDelta,
    queue_wait_ticks: CounterDelta,
    service_samples: CounterDelta,
    service_ticks: CounterDelta,
}

impl ResourceLockIoDelta {
    const fn new() -> Self {
        Self {
            submits: CounterDelta::new(),
            bytes: CounterDelta::new(),
            queue_wait_samples: CounterDelta::new(),
            queue_wait_ticks: CounterDelta::new(),
            service_samples: CounterDelta::new(),
            service_ticks: CounterDelta::new(),
        }
    }
}

struct DurationCounterDelta {
    samples: CounterDelta,
    ticks: CounterDelta,
}

impl DurationCounterDelta {
    const fn new() -> Self {
        Self {
            samples: CounterDelta::new(),
            ticks: CounterDelta::new(),
        }
    }
}

impl PhaseDelta {
    const fn new() -> Self {
        Self {
            samples: AtomicUsize::new(0),
            ticks: AtomicUsize::new(0),
        }
    }
}

/// Print a module banner so cumulative output can be split by subsystem.
fn emit_section(name: &str) {
    println!("### {} PERF ###", name);
}

fn take_delta(last: &AtomicUsize, counter: &AtomicUsize) -> usize {
    let current = counter.load(Ordering::Relaxed);
    let previous = last.swap(current, Ordering::Relaxed);
    current.saturating_sub(previous)
}

static DELTA_REPORT_MS: AtomicUsize = AtomicUsize::new(0);

static DELTA_SYSCALL_READ: DurationDelta = DurationDelta::new();
static DELTA_SYSCALL_WRITE: DurationDelta = DurationDelta::new();
static DELTA_SYSCALL_OPEN: DurationDelta = DurationDelta::new();
static DELTA_SYSCALL_STAT: DurationDelta = DurationDelta::new();
static DELTA_SYSCALL_PATH: DurationDelta = DurationDelta::new();
static DELTA_PIPE_READ_WAIT: DurationDelta = DurationDelta::new();
static DELTA_SCHEDULER_DISPATCH: DurationDelta = DurationDelta::new();

static DELTA_EXT4_WRITE_OPEN: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_WRITE_QUOTA: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_WRITE_DATA: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_FSTAT_ACTUAL: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_FSTAT_INNER_COLD: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_FSTAT_INNER_DENSE: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_FSTAT_INNER_SPARSE: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_RENAME_WRITE_BACK: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_RENAME_DENSE: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_NAMESPACE_CREATE: PhaseDelta = PhaseDelta::new();
static DELTA_EXT4_NAMESPACE_UNLINK: PhaseDelta = PhaseDelta::new();

static DELTA_SCHED_LOCAL_ENQUEUES: CounterDelta = CounterDelta::new();
static DELTA_SCHED_REMOTE_ENQUEUES: CounterDelta = CounterDelta::new();
static DELTA_SCHED_REMOTE_IDLE_NOTIFICATIONS: CounterDelta = CounterDelta::new();
static DELTA_SCHED_REMOTE_IPI_SENT: CounterDelta = CounterDelta::new();
static DELTA_SCHED_REMOTE_IPI_FAILED: CounterDelta = CounterDelta::new();
static DELTA_PIPE_READER_WAKE_CALLS: CounterDelta = CounterDelta::new();
static DELTA_PIPE_READER_WAKE_TASKS: CounterDelta = CounterDelta::new();
static DELTA_PIPE_READER_WAKE_POLL_TASKS: CounterDelta = CounterDelta::new();
static DELTA_PIPE_READ_WAIT_RECHECKS: CounterDelta = CounterDelta::new();
static DELTA_PIPE_WRITER_WAKE_CALLS: CounterDelta = CounterDelta::new();
static DELTA_PIPE_WRITER_WAKE_TASKS: CounterDelta = CounterDelta::new();
static DELTA_PIPE_WRITER_WAKE_POLL_TASKS: CounterDelta = CounterDelta::new();
static DELTA_PIPE_WRITE_WAIT_RECHECKS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_RESOURCE_LOCK_CLASSES: [ResourceLockClassDelta; 9] =
    [const { ResourceLockClassDelta::new() }; 9];
#[cfg(feature = "perf")]
static DELTA_RESOURCE_LOCK_IO: [ResourceLockIoDelta; 27] =
    [const { ResourceLockIoDelta::new() }; 27];
#[cfg(feature = "perf")]
static DELTA_RESOURCE_LOCK_BCACHE_WAIT: [DurationCounterDelta; 9] =
    [const { DurationCounterDelta::new() }; 9];

#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_HIT_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_HIT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_FAST_HIT_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_FAST_HIT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_INIT_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_INIT_READ_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_EVICT_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_EVICT_WRITEBACK_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_LIMIT_FLUSH_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_CACHE_LIMIT_FLUSH_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_WRITE_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_WRITE_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_DISABLED_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_DISABLED_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_TOO_LARGE_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_TOO_LARGE_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_UNCACHED_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_UNCACHED_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_HOLE_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_HOLE_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_LIMIT_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_DIRECT_LIMIT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_BUFFER_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_BUFFER_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_FSTAT_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_FSTAT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_CLOSE_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_CLOSE_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_RENAME_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_RENAME_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_TRUNCATE_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_TRUNCATE_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_CACHE_EVICT_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_CACHE_EVICT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_OTHER_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_OTHER_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_FSTAT_CALLS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_FSTAT_SPARSE_FLUSH_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_FSTAT_SPARSE_FLUSH_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_FSTAT_STAT_GET_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_FSTAT_WRITE_BACK_OVERLAY_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_FSTAT_WRITE_BACK_FALLBACK_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_RENAME_WRITE_BACK_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_RENAME_SPARSE_FLUSH_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_RENAME_DENSE_WRITE_BACK_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_RENAME_ZERO_BYTE_FAST_PATH_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_RENAME_PATH_CACHE_DISCARD_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_BATCHES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_BATCH_RUNS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_FLUSH_BATCH_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BATCHES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_RUN_LIMIT_BATCHES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_RUN_LIMIT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_BOTH_LIMIT_BATCHES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_BOTH_LIMIT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_ALLOC_FAILURE_BATCHES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_ALLOC_FAILURE_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_LARGE_DIRECT_BATCHES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_LARGE_DIRECT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BATCHES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_BUFFER_ALLOC_FAILURE_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_BUFFER_ALLOC_FAILURE_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_LARGE_DIRECT_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_LARGE_DIRECT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_BUFFER_BUDGET_DIRECT_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_BUFFER_BUDGET_DIRECT_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_READ_OVERLAY_OPS: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_READ_OVERLAY_BYTES: CounterDelta = CounterDelta::new();
#[cfg(feature = "perf")]
static DELTA_EXT4_SPARSE_READ_OVERLAY_DIRTY_BYTES: CounterDelta = CounterDelta::new();

#[cfg(feature = "perf")]
macro_rules! storage_counter_deltas {
    ($($name:ident),+ $(,)?) => {
        $(static $name: CounterDelta = CounterDelta::new();)+
    };
}

#[cfg(feature = "perf")]
storage_counter_deltas!(
    DELTA_BCACHE_GET_OPS,
    DELTA_BCACHE_CACHE_HITS,
    DELTA_BCACHE_CACHE_MISSES,
    DELTA_BCACHE_ALLOCATIONS,
    DELTA_BCACHE_ALLOCATION_RACES,
    DELTA_BCACHE_LOADER_OPS,
    DELTA_BCACHE_LOADER_SUCCESSES,
    DELTA_BCACHE_LOADER_ERRORS,
    DELTA_BCACHE_WAIT_OPS,
    DELTA_BCACHE_WAIT_RECHECKS,
    DELTA_BCACHE_WAKE_CALLS,
    DELTA_BCACHE_SHAKE_CALLS,
    DELTA_BCACHE_CLEAN_EVICTIONS,
    DELTA_BCACHE_SHAKE_FULL_DIRTY,
    DELTA_BCACHE_SHAKE_FULL_PINNED,
    DELTA_BCACHE_CAPACITY_OVERFLOWS,
    DELTA_BCACHE_DIRTY_CAPACITY_RECLAIM_RUNS,
    DELTA_BCACHE_DIRTY_CAPACITY_RECLAIMED_BLOCKS,
    DELTA_BCACHE_DIRTY_CAPACITY_RECLAIM_STALLS,
    DELTA_BCACHE_WRITEBACK_OPS,
    DELTA_BCACHE_WRITEBACK_SUCCESSES,
    DELTA_BCACHE_WRITEBACK_ERRORS,
    DELTA_BCACHE_WRITEBACK_WAITS,
    DELTA_BCACHE_DROPS,
    DELTA_BCACHE_READ_SUBMITS,
    DELTA_BCACHE_READ_COMPLETIONS,
    DELTA_BCACHE_READ_BLOCKS,
    DELTA_BCACHE_READ_ERRORS,
    DELTA_BCACHE_WRITE_SUBMITS,
    DELTA_BCACHE_WRITE_COMPLETIONS,
    DELTA_BCACHE_WRITE_BLOCKS,
    DELTA_BCACHE_WRITE_ERRORS,
    DELTA_BCACHE_WAIT_SAMPLES,
    DELTA_BCACHE_WAIT_TICKS,
    DELTA_BCACHE_LOCKED_WAIT_SAMPLES,
    DELTA_BCACHE_LOCKED_WAIT_TICKS,
    DELTA_BCACHE_UNLOCKED_WAIT_SAMPLES,
    DELTA_BCACHE_UNLOCKED_WAIT_TICKS,
    DELTA_BLOCKDEV_SUBMITS,
    DELTA_BLOCKDEV_READ_REQUESTS,
    DELTA_BLOCKDEV_WRITE_REQUESTS,
    DELTA_BLOCKDEV_FLUSH_REQUESTS,
    DELTA_BLOCKDEV_COMPLETED,
    DELTA_BLOCKDEV_CONTENDED,
    DELTA_BLOCKDEV_QUEUED,
    DELTA_BLOCKDEV_BYTES,
    DELTA_BLOCKDEV_ERRORS,
    DELTA_BLOCKDEV_WAIT_TICKS,
    DELTA_BLOCKDEV_SERVICE_TICKS,
    DELTA_BLOCKDEV_ALIGNED_REQUESTS,
    DELTA_BLOCKDEV_UNALIGNED_REQUESTS,
    DELTA_BLOCKDEV_SEQUENTIAL_REQUESTS,
    DELTA_RESOURCE_LOCK_ACQUIRES,
    DELTA_RESOURCE_LOCK_CONTENDED,
    DELTA_RESOURCE_LOCK_QUEUED,
    DELTA_RESOURCE_LOCK_WAIT_TICKS,
    DELTA_RESOURCE_LOCK_HOLD_TICKS,
    DELTA_RESOURCE_REGISTRY_LOOKUPS,
    DELTA_RESOURCE_REGISTRY_CREATES,
    DELTA_RESOURCE_REGISTRY_TICKS,
);

fn emit_duration_delta(
    label: &str,
    samples: &AtomicUsize,
    ticks: &AtomicUsize,
    delta: &DurationDelta,
) {
    let sample_delta = take_delta(&delta.samples, samples);
    let tick_delta = take_delta(&delta.ticks, ticks);
    println!(
        "[perf] interval_duration name={} samples={} total_us={}",
        label,
        sample_delta,
        ticks_to_us(tick_delta),
    );
}

fn emit_phase_delta(label: &str, stats: &Ext4PhaseStats, delta: &PhaseDelta) {
    let samples = take_delta(&delta.samples, &stats.samples);
    let ticks = take_delta(&delta.ticks, &stats.ticks);
    println!(
        "[perf] interval_ext4_phase name={} samples={} total_us={} max_us={}",
        label,
        samples,
        ticks_to_us(ticks),
        ticks_to_us(stats.max_ticks.load(Ordering::Relaxed)),
    );
}

#[cfg(feature = "perf")]
fn emit_resource_lock_class_interval_deltas() {
    for class in Ext4ResourceLockClass::ALL {
        let stats = ext4_resource_lock_class_stats(class);
        let delta = &DELTA_RESOURCE_LOCK_CLASSES[class.index()];
        println!(
            "[perf] interval_ext4_resource_lock class={} acquires={} contended={} queued={} max_queue_depth={} wait_us={} max_wait_us={} hold_us={} max_hold_us={}",
            class.label(),
            delta.acquires.take(&stats.acquires),
            delta.contended.take(&stats.contended),
            delta.queued.take(&stats.queued),
            stats.max_queue_depth.load(Ordering::Relaxed),
            ticks_to_us(delta.wait_ticks.take(&stats.wait_ticks)),
            ticks_to_us(stats.max_wait_ticks.load(Ordering::Relaxed)),
            ticks_to_us(delta.hold_ticks.take(&stats.hold_ticks)),
            ticks_to_us(stats.max_hold_ticks.load(Ordering::Relaxed)),
        );
    }
}

#[cfg(feature = "perf")]
fn emit_resource_lock_io_interval_deltas() {
    for class in Ext4ResourceLockClass::ALL {
        for kind in Ext4BlockRequestKind::ALL {
            let stats = ext4_resource_lock_io_stats(class, kind);
            let delta = &DELTA_RESOURCE_LOCK_IO
                [class.index() * Ext4BlockRequestKind::ALL.len() + kind.index()];
            let submits = delta.submits.take(&stats.submits);
            let bytes = delta.bytes.take(&stats.bytes);
            let queue_wait_samples = delta.queue_wait_samples.take(&stats.queue_wait_samples);
            let queue_wait_ticks = delta.queue_wait_ticks.take(&stats.queue_wait_ticks);
            let service_samples = delta.service_samples.take(&stats.service_samples);
            let service_ticks = delta.service_ticks.take(&stats.service_ticks);
            if submits == 0 && queue_wait_samples == 0 && service_samples == 0 {
                continue;
            }
            println!(
                "[perf] interval_ext4_lock_io lock_class={} request={} submits={} bytes={} queue_wait_samples={} queue_wait_us={} max_queue_wait_us={} service_samples={} service_us={} max_service_us={}",
                class.label(),
                kind.label(),
                submits,
                bytes,
                queue_wait_samples,
                ticks_to_us(queue_wait_ticks),
                ticks_to_us(stats.max_queue_wait_ticks.load(Ordering::Relaxed)),
                service_samples,
                ticks_to_us(service_ticks),
                ticks_to_us(stats.max_service_ticks.load(Ordering::Relaxed)),
            );
        }

        let stats = ext4_resource_lock_bcache_wait_stats(class);
        let delta = &DELTA_RESOURCE_LOCK_BCACHE_WAIT[class.index()];
        let samples = delta.samples.take(&stats.samples);
        let ticks = delta.ticks.take(&stats.ticks);
        if samples != 0 {
            println!(
                "[perf] interval_ext4_lock_bcache_wait lock_class={} samples={} wait_us={} max_wait_us={}",
                class.label(),
                samples,
                ticks_to_us(ticks),
                ticks_to_us(stats.max_ticks.load(Ordering::Relaxed)),
            );
        }
    }
}

#[cfg(feature = "perf")]
fn emit_resource_lock_class_cumulative() {
    for class in Ext4ResourceLockClass::ALL {
        let stats = ext4_resource_lock_class_stats(class);
        println!(
            "[perf] ext4_resource_lock class={} acquires={} contended={} queued={} max_queue_depth={} wait_us={} max_wait_us={} hold_us={} max_hold_us={}",
            class.label(),
            stats.acquires.load(Ordering::Relaxed),
            stats.contended.load(Ordering::Relaxed),
            stats.queued.load(Ordering::Relaxed),
            stats.max_queue_depth.load(Ordering::Relaxed),
            ticks_to_us(stats.wait_ticks.load(Ordering::Relaxed)),
            ticks_to_us(stats.max_wait_ticks.load(Ordering::Relaxed)),
            ticks_to_us(stats.hold_ticks.load(Ordering::Relaxed)),
            ticks_to_us(stats.max_hold_ticks.load(Ordering::Relaxed)),
        );
    }
}

#[cfg(feature = "perf")]
fn emit_resource_lock_io_cumulative() {
    for class in Ext4ResourceLockClass::ALL {
        for kind in Ext4BlockRequestKind::ALL {
            let stats = ext4_resource_lock_io_stats(class, kind);
            if stats.submits.load(Ordering::Relaxed) == 0 {
                continue;
            }
            println!(
                "[perf] ext4_lock_io lock_class={} request={} submits={} bytes={} queue_wait_samples={} queue_wait_us={} max_queue_wait_us={} service_samples={} service_us={} max_service_us={}",
                class.label(),
                kind.label(),
                stats.submits.load(Ordering::Relaxed),
                stats.bytes.load(Ordering::Relaxed),
                stats.queue_wait_samples.load(Ordering::Relaxed),
                ticks_to_us(stats.queue_wait_ticks.load(Ordering::Relaxed)),
                ticks_to_us(stats.max_queue_wait_ticks.load(Ordering::Relaxed)),
                stats.service_samples.load(Ordering::Relaxed),
                ticks_to_us(stats.service_ticks.load(Ordering::Relaxed)),
                ticks_to_us(stats.max_service_ticks.load(Ordering::Relaxed)),
            );
        }

        let stats = ext4_resource_lock_bcache_wait_stats(class);
        if stats.samples.load(Ordering::Relaxed) != 0 {
            println!(
                "[perf] ext4_lock_bcache_wait lock_class={} samples={} wait_us={} max_wait_us={}",
                class.label(),
                stats.samples.load(Ordering::Relaxed),
                ticks_to_us(stats.ticks.load(Ordering::Relaxed)),
                ticks_to_us(stats.max_ticks.load(Ordering::Relaxed)),
            );
        }
    }
}

fn emit_raw_phase_delta(
    label: &str,
    samples: &AtomicUsize,
    ticks: &AtomicUsize,
    max_ticks: &AtomicUsize,
    delta: &PhaseDelta,
) {
    let sample_delta = take_delta(&delta.samples, samples);
    let tick_delta = take_delta(&delta.ticks, ticks);
    println!(
        "[perf] interval_ext4_phase name={} samples={} total_us={} max_us={}",
        label,
        sample_delta,
        ticks_to_us(tick_delta),
        ticks_to_us(max_ticks.load(Ordering::Relaxed)),
    );
}

#[cfg(feature = "perf")]
fn emit_write_cache_interval_deltas() {
    let write_cache = lwext4_rust::perf::write_back_cache_perf_stats();
    println!(
        "[perf] interval_ext4_write_cache hit_ops={} hit_bytes={} fast_hit_ops={} fast_hit_bytes={} init_ops={} init_read_bytes={} evict_ops={} evict_writeback_bytes={} limit_flush_ops={} limit_flush_bytes={} direct_ops={} direct_bytes={} direct_disabled_ops={} direct_disabled_bytes={} direct_too_large_ops={} direct_too_large_bytes={} direct_uncached_ops={} direct_uncached_bytes={} direct_hole_ops={} direct_hole_bytes={} direct_limit_ops={} direct_limit_bytes={} sparse_buffer_ops={} sparse_buffer_bytes={} sparse_flush_ops={} sparse_flush_bytes={} sparse_flush_fstat_ops={} sparse_flush_fstat_bytes={} sparse_flush_close_ops={} sparse_flush_close_bytes={} sparse_flush_rename_ops={} sparse_flush_rename_bytes={} sparse_flush_truncate_ops={} sparse_flush_truncate_bytes={} sparse_flush_cache_evict_ops={} sparse_flush_cache_evict_bytes={} sparse_flush_other_ops={} sparse_flush_other_bytes={} sparse_read_overlay_ops={} sparse_read_overlay_bytes={} sparse_read_overlay_dirty_bytes={}",
        DELTA_EXT4_CACHE_HIT_OPS.take_value(write_cache.cache_hit_ops),
        DELTA_EXT4_CACHE_HIT_BYTES.take_value(write_cache.cache_hit_bytes),
        DELTA_EXT4_CACHE_FAST_HIT_OPS.take_value(write_cache.cache_fast_hit_ops),
        DELTA_EXT4_CACHE_FAST_HIT_BYTES.take_value(write_cache.cache_fast_hit_bytes),
        DELTA_EXT4_CACHE_INIT_OPS.take_value(write_cache.cache_init_ops),
        DELTA_EXT4_CACHE_INIT_READ_BYTES.take_value(write_cache.cache_init_read_bytes),
        DELTA_EXT4_CACHE_EVICT_OPS.take_value(write_cache.cache_evict_ops),
        DELTA_EXT4_CACHE_EVICT_WRITEBACK_BYTES.take_value(write_cache.cache_evict_writeback_bytes),
        DELTA_EXT4_CACHE_LIMIT_FLUSH_OPS.take_value(write_cache.cache_limit_flush_ops),
        DELTA_EXT4_CACHE_LIMIT_FLUSH_BYTES.take_value(write_cache.cache_limit_flush_bytes),
        DELTA_EXT4_DIRECT_WRITE_OPS.take_value(write_cache.direct_write_ops),
        DELTA_EXT4_DIRECT_WRITE_BYTES.take_value(write_cache.direct_write_bytes),
        DELTA_EXT4_DIRECT_DISABLED_OPS.take_value(write_cache.direct_disabled_ops),
        DELTA_EXT4_DIRECT_DISABLED_BYTES.take_value(write_cache.direct_disabled_bytes),
        DELTA_EXT4_DIRECT_TOO_LARGE_OPS.take_value(write_cache.direct_too_large_ops),
        DELTA_EXT4_DIRECT_TOO_LARGE_BYTES.take_value(write_cache.direct_too_large_bytes),
        DELTA_EXT4_DIRECT_UNCACHED_OPS.take_value(write_cache.direct_uncached_ops),
        DELTA_EXT4_DIRECT_UNCACHED_BYTES.take_value(write_cache.direct_uncached_bytes),
        DELTA_EXT4_DIRECT_HOLE_OPS.take_value(write_cache.direct_hole_ops),
        DELTA_EXT4_DIRECT_HOLE_BYTES.take_value(write_cache.direct_hole_bytes),
        DELTA_EXT4_DIRECT_LIMIT_OPS.take_value(write_cache.direct_limit_ops),
        DELTA_EXT4_DIRECT_LIMIT_BYTES.take_value(write_cache.direct_limit_bytes),
        DELTA_EXT4_SPARSE_BUFFER_OPS.take_value(write_cache.sparse_buffer_ops),
        DELTA_EXT4_SPARSE_BUFFER_BYTES.take_value(write_cache.sparse_buffer_bytes),
        DELTA_EXT4_SPARSE_FLUSH_OPS.take_value(write_cache.sparse_flush_ops),
        DELTA_EXT4_SPARSE_FLUSH_BYTES.take_value(write_cache.sparse_flush_bytes),
        DELTA_EXT4_SPARSE_FLUSH_FSTAT_OPS.take_value(write_cache.sparse_flush_fstat_ops),
        DELTA_EXT4_SPARSE_FLUSH_FSTAT_BYTES.take_value(write_cache.sparse_flush_fstat_bytes),
        DELTA_EXT4_SPARSE_FLUSH_CLOSE_OPS.take_value(write_cache.sparse_flush_close_ops),
        DELTA_EXT4_SPARSE_FLUSH_CLOSE_BYTES.take_value(write_cache.sparse_flush_close_bytes),
        DELTA_EXT4_SPARSE_FLUSH_RENAME_OPS.take_value(write_cache.sparse_flush_rename_ops),
        DELTA_EXT4_SPARSE_FLUSH_RENAME_BYTES.take_value(write_cache.sparse_flush_rename_bytes),
        DELTA_EXT4_SPARSE_FLUSH_TRUNCATE_OPS.take_value(write_cache.sparse_flush_truncate_ops),
        DELTA_EXT4_SPARSE_FLUSH_TRUNCATE_BYTES.take_value(write_cache.sparse_flush_truncate_bytes),
        DELTA_EXT4_SPARSE_FLUSH_CACHE_EVICT_OPS
            .take_value(write_cache.sparse_flush_cache_evict_ops),
        DELTA_EXT4_SPARSE_FLUSH_CACHE_EVICT_BYTES
            .take_value(write_cache.sparse_flush_cache_evict_bytes),
        DELTA_EXT4_SPARSE_FLUSH_OTHER_OPS.take_value(write_cache.sparse_flush_other_ops),
        DELTA_EXT4_SPARSE_FLUSH_OTHER_BYTES.take_value(write_cache.sparse_flush_other_bytes),
        DELTA_EXT4_SPARSE_READ_OVERLAY_OPS.take_value(write_cache.sparse_read_overlay_ops),
        DELTA_EXT4_SPARSE_READ_OVERLAY_BYTES.take_value(write_cache.sparse_read_overlay_bytes),
        DELTA_EXT4_SPARSE_READ_OVERLAY_DIRTY_BYTES
            .take_value(write_cache.sparse_read_overlay_dirty_bytes),
    );
    println!(
        "[perf] interval_ext4_fstat_inner calls={} sparse_flush_batches={} sparse_flush_bytes={} stat_get_ops={} write_back_overlay_ops={} write_back_fallback_ops={}",
        DELTA_EXT4_FSTAT_CALLS.take_value(write_cache.fstat_calls),
        DELTA_EXT4_FSTAT_SPARSE_FLUSH_OPS.take_value(write_cache.sparse_flush_fstat_ops),
        DELTA_EXT4_FSTAT_SPARSE_FLUSH_BYTES.take_value(write_cache.sparse_flush_fstat_bytes),
        DELTA_EXT4_FSTAT_STAT_GET_OPS.take_value(write_cache.fstat_stat_get_ops),
        DELTA_EXT4_FSTAT_WRITE_BACK_OVERLAY_OPS
            .take_value(write_cache.fstat_write_back_overlay_ops),
        DELTA_EXT4_FSTAT_WRITE_BACK_FALLBACK_OPS
            .take_value(write_cache.fstat_write_back_fallback_ops),
    );
    println!(
        "[perf] interval_ext4_rename_write_back ops={} sparse_flush_bytes={} dense_write_back_bytes={} zero_byte_fast_path_ops={} path_cache_discard_ops={}",
        DELTA_EXT4_RENAME_WRITE_BACK_OPS.take_value(write_cache.rename_write_back_ops),
        DELTA_EXT4_RENAME_SPARSE_FLUSH_BYTES.take_value(write_cache.rename_sparse_flush_bytes),
        DELTA_EXT4_RENAME_DENSE_WRITE_BACK_BYTES
            .take_value(write_cache.rename_dense_write_back_bytes),
        DELTA_EXT4_RENAME_ZERO_BYTE_FAST_PATH_OPS
            .take_value(write_cache.rename_zero_byte_fast_path_ops),
        DELTA_EXT4_RENAME_PATH_CACHE_DISCARD_OPS
            .take_value(write_cache.rename_path_cache_discard_ops),
    );
    println!(
        "[perf] interval_ext4_sparse_buffer batches={} batch_runs={} batch_bytes={} batch_max_runs={} batch_max_bytes={} payload_limit_batches={} payload_limit_bytes={} run_limit_batches={} run_limit_bytes={} both_limits_batches={} both_limits_bytes={} allocation_failure_batches={} allocation_failure_bytes={} large_direct_batches={} large_direct_bytes={} global_budget_batches={} global_budget_bytes={} allocation_failure_ops={} allocation_failure_request_bytes={} large_direct_ops={} large_direct_request_bytes={} global_budget_direct_ops={} global_budget_direct_request_bytes={} resident_max_bytes={}",
        DELTA_EXT4_SPARSE_FLUSH_BATCHES.take_value(write_cache.sparse_flush_batches),
        DELTA_EXT4_SPARSE_FLUSH_BATCH_RUNS.take_value(write_cache.sparse_flush_batch_runs),
        DELTA_EXT4_SPARSE_FLUSH_BATCH_BYTES.take_value(write_cache.sparse_flush_batch_bytes),
        write_cache.sparse_flush_batch_max_runs,
        write_cache.sparse_flush_batch_max_bytes,
        DELTA_EXT4_SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BATCHES
            .take_value(write_cache.sparse_cache_evict_payload_limit_batches),
        DELTA_EXT4_SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BYTES
            .take_value(write_cache.sparse_cache_evict_payload_limit_bytes),
        DELTA_EXT4_SPARSE_CACHE_EVICT_RUN_LIMIT_BATCHES
            .take_value(write_cache.sparse_cache_evict_run_limit_batches),
        DELTA_EXT4_SPARSE_CACHE_EVICT_RUN_LIMIT_BYTES
            .take_value(write_cache.sparse_cache_evict_run_limit_bytes),
        DELTA_EXT4_SPARSE_CACHE_EVICT_BOTH_LIMIT_BATCHES
            .take_value(write_cache.sparse_cache_evict_both_limits_batches),
        DELTA_EXT4_SPARSE_CACHE_EVICT_BOTH_LIMIT_BYTES
            .take_value(write_cache.sparse_cache_evict_both_limits_bytes),
        DELTA_EXT4_SPARSE_CACHE_EVICT_ALLOC_FAILURE_BATCHES
            .take_value(write_cache.sparse_cache_evict_allocation_failure_batches),
        DELTA_EXT4_SPARSE_CACHE_EVICT_ALLOC_FAILURE_BYTES
            .take_value(write_cache.sparse_cache_evict_allocation_failure_bytes),
        DELTA_EXT4_SPARSE_CACHE_EVICT_LARGE_DIRECT_BATCHES
            .take_value(write_cache.sparse_cache_evict_large_direct_batches),
        DELTA_EXT4_SPARSE_CACHE_EVICT_LARGE_DIRECT_BYTES
            .take_value(write_cache.sparse_cache_evict_large_direct_bytes),
        DELTA_EXT4_SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BATCHES
            .take_value(write_cache.sparse_cache_evict_global_budget_batches),
        DELTA_EXT4_SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BYTES
            .take_value(write_cache.sparse_cache_evict_global_budget_bytes),
        DELTA_EXT4_SPARSE_BUFFER_ALLOC_FAILURE_OPS
            .take_value(write_cache.sparse_buffer_allocation_failure_ops),
        DELTA_EXT4_SPARSE_BUFFER_ALLOC_FAILURE_BYTES
            .take_value(write_cache.sparse_buffer_allocation_failure_bytes),
        DELTA_EXT4_SPARSE_LARGE_DIRECT_OPS.take_value(write_cache.sparse_large_direct_ops),
        DELTA_EXT4_SPARSE_LARGE_DIRECT_BYTES.take_value(write_cache.sparse_large_direct_bytes),
        DELTA_EXT4_SPARSE_BUFFER_BUDGET_DIRECT_OPS
            .take_value(write_cache.sparse_buffer_budget_direct_ops),
        DELTA_EXT4_SPARSE_BUFFER_BUDGET_DIRECT_BYTES
            .take_value(write_cache.sparse_buffer_budget_direct_bytes),
        write_cache.sparse_buffer_resident_max_bytes,
    );
}

#[cfg(feature = "perf")]
fn emit_ext4_storage_interval_deltas() {
    let bcache = lwext4_rust::perf::bcache_perf_stats();
    println!(
        "[perf] interval_ext4_bcache get_ops={} cache_hits={} cache_misses={} allocations={} allocation_races={} loader_ops={} loader_successes={} loader_errors={} wait_ops={} wait_rechecks={} wake_calls={} shake_calls={} clean_evictions={} shake_full_dirty={} shake_full_pinned={} capacity_overflows={} dirty_capacity_reclaim_runs={} dirty_capacity_reclaimed_blocks={} dirty_capacity_reclaim_stalls={} writeback_ops={} writeback_successes={} writeback_errors={} writeback_waits={} drops={} initial_resident_blocks={} resident_blocks={} max_resident_blocks={}",
        DELTA_BCACHE_GET_OPS.take_value(bcache.get_ops),
        DELTA_BCACHE_CACHE_HITS.take_value(bcache.cache_hits),
        DELTA_BCACHE_CACHE_MISSES.take_value(bcache.cache_misses),
        DELTA_BCACHE_ALLOCATIONS.take_value(bcache.allocations),
        DELTA_BCACHE_ALLOCATION_RACES.take_value(bcache.allocation_races),
        DELTA_BCACHE_LOADER_OPS.take_value(bcache.loader_ops),
        DELTA_BCACHE_LOADER_SUCCESSES.take_value(bcache.loader_successes),
        DELTA_BCACHE_LOADER_ERRORS.take_value(bcache.loader_errors),
        DELTA_BCACHE_WAIT_OPS.take_value(bcache.wait_ops),
        DELTA_BCACHE_WAIT_RECHECKS.take_value(bcache.wait_rechecks),
        DELTA_BCACHE_WAKE_CALLS.take_value(bcache.wake_calls),
        DELTA_BCACHE_SHAKE_CALLS.take_value(bcache.shake_calls),
        DELTA_BCACHE_CLEAN_EVICTIONS.take_value(bcache.clean_evictions),
        DELTA_BCACHE_SHAKE_FULL_DIRTY.take_value(bcache.shake_full_dirty),
        DELTA_BCACHE_SHAKE_FULL_PINNED.take_value(bcache.shake_full_pinned),
        DELTA_BCACHE_CAPACITY_OVERFLOWS.take_value(bcache.capacity_overflows),
        DELTA_BCACHE_DIRTY_CAPACITY_RECLAIM_RUNS
            .take_value(bcache.dirty_capacity_reclaim_runs),
        DELTA_BCACHE_DIRTY_CAPACITY_RECLAIMED_BLOCKS
            .take_value(bcache.dirty_capacity_reclaimed_blocks),
        DELTA_BCACHE_DIRTY_CAPACITY_RECLAIM_STALLS
            .take_value(bcache.dirty_capacity_reclaim_stalls),
        DELTA_BCACHE_WRITEBACK_OPS.take_value(bcache.writeback_ops),
        DELTA_BCACHE_WRITEBACK_SUCCESSES.take_value(bcache.writeback_successes),
        DELTA_BCACHE_WRITEBACK_ERRORS.take_value(bcache.writeback_errors),
        DELTA_BCACHE_WRITEBACK_WAITS.take_value(bcache.writeback_waits),
        DELTA_BCACHE_DROPS.take_value(bcache.drops),
        bcache.initial_resident_blocks,
        bcache.resident_blocks,
        bcache.max_resident_blocks,
    );
    println!(
        "[perf] interval_ext4_bcache_io read_submits={} read_completions={} read_blocks={} read_errors={} write_submits={} write_completions={} write_blocks={} write_errors={}",
        DELTA_BCACHE_READ_SUBMITS.take_value(bcache.read_submits),
        DELTA_BCACHE_READ_COMPLETIONS.take_value(bcache.read_completions),
        DELTA_BCACHE_READ_BLOCKS.take_value(bcache.read_blocks),
        DELTA_BCACHE_READ_ERRORS.take_value(bcache.read_errors),
        DELTA_BCACHE_WRITE_SUBMITS.take_value(bcache.write_submits),
        DELTA_BCACHE_WRITE_COMPLETIONS.take_value(bcache.write_completions),
        DELTA_BCACHE_WRITE_BLOCKS.take_value(bcache.write_blocks),
        DELTA_BCACHE_WRITE_ERRORS.take_value(bcache.write_errors),
    );
    println!(
        "[perf] interval_ext4_block_device submits={} read_requests={} write_requests={} flush_requests={} completed={} contended={} queued={} max_queue_depth={} bytes={} errors={} wait_us={} max_wait_us={} service_us={} max_service_us={} aligned_requests={} unaligned_requests={} sequential_requests={} max_request_bytes={}",
        DELTA_BLOCKDEV_SUBMITS.take(&EXT4_BLOCK_DEVICE_STATS.submits),
        DELTA_BLOCKDEV_READ_REQUESTS.take(&EXT4_BLOCK_DEVICE_STATS.read_requests),
        DELTA_BLOCKDEV_WRITE_REQUESTS.take(&EXT4_BLOCK_DEVICE_STATS.write_requests),
        DELTA_BLOCKDEV_FLUSH_REQUESTS.take(&EXT4_BLOCK_DEVICE_STATS.flush_requests),
        DELTA_BLOCKDEV_COMPLETED.take(&EXT4_BLOCK_DEVICE_STATS.completed),
        DELTA_BLOCKDEV_CONTENDED.take(&EXT4_BLOCK_DEVICE_STATS.contended),
        DELTA_BLOCKDEV_QUEUED.take(&EXT4_BLOCK_DEVICE_STATS.queued),
        EXT4_BLOCK_DEVICE_STATS
            .max_queue_depth
            .load(Ordering::Relaxed),
        DELTA_BLOCKDEV_BYTES.take(&EXT4_BLOCK_DEVICE_STATS.bytes),
        DELTA_BLOCKDEV_ERRORS.take(&EXT4_BLOCK_DEVICE_STATS.errors),
        ticks_to_us(DELTA_BLOCKDEV_WAIT_TICKS.take(&EXT4_BLOCK_DEVICE_STATS.wait_ticks)),
        ticks_to_us(EXT4_BLOCK_DEVICE_STATS.max_wait_ticks.load(Ordering::Relaxed)),
        ticks_to_us(
            DELTA_BLOCKDEV_SERVICE_TICKS.take(&EXT4_BLOCK_DEVICE_STATS.service_ticks)
        ),
        ticks_to_us(
            EXT4_BLOCK_DEVICE_STATS
                .max_service_ticks
                .load(Ordering::Relaxed)
        ),
        DELTA_BLOCKDEV_ALIGNED_REQUESTS.take(&EXT4_BLOCK_DEVICE_STATS.aligned_requests),
        DELTA_BLOCKDEV_UNALIGNED_REQUESTS.take(&EXT4_BLOCK_DEVICE_STATS.unaligned_requests),
        DELTA_BLOCKDEV_SEQUENTIAL_REQUESTS.take(&EXT4_BLOCK_DEVICE_STATS.sequential_requests),
        EXT4_BLOCK_DEVICE_STATS
            .max_request_bytes
            .load(Ordering::Relaxed),
    );
    println!(
        "[perf] interval_ext4_bcache_completion_wait samples={} wait_us={} max_wait_us={} lock_held_samples={} lock_held_wait_us={} unlocked_samples={} unlocked_wait_us={}",
        DELTA_BCACHE_WAIT_SAMPLES.take(&EXT4_BCACHE_WAIT_SAMPLES),
        ticks_to_us(DELTA_BCACHE_WAIT_TICKS.take(&EXT4_BCACHE_WAIT_TICKS)),
        ticks_to_us(EXT4_BCACHE_WAIT_MAX_TICKS.load(Ordering::Relaxed)),
        DELTA_BCACHE_LOCKED_WAIT_SAMPLES.take(&EXT4_BCACHE_LOCKED_WAIT.samples),
        ticks_to_us(DELTA_BCACHE_LOCKED_WAIT_TICKS.take(&EXT4_BCACHE_LOCKED_WAIT.ticks)),
        DELTA_BCACHE_UNLOCKED_WAIT_SAMPLES.take(&EXT4_BCACHE_UNLOCKED_WAIT.samples),
        ticks_to_us(DELTA_BCACHE_UNLOCKED_WAIT_TICKS.take(&EXT4_BCACHE_UNLOCKED_WAIT.ticks)),
    );
    println!(
        "[perf] interval_ext4_resource_locks acquires={} contended={} queued={} max_queue_depth={} wait_us={} max_wait_us={} hold_us={} max_hold_us={}",
        DELTA_RESOURCE_LOCK_ACQUIRES.take(&EXT4_RESOURCE_LOCK_STATS.acquires),
        DELTA_RESOURCE_LOCK_CONTENDED.take(&EXT4_RESOURCE_LOCK_STATS.contended),
        DELTA_RESOURCE_LOCK_QUEUED.take(&EXT4_RESOURCE_LOCK_STATS.queued),
        EXT4_RESOURCE_LOCK_STATS
            .max_queue_depth
            .load(Ordering::Relaxed),
        ticks_to_us(DELTA_RESOURCE_LOCK_WAIT_TICKS.take(&EXT4_RESOURCE_LOCK_STATS.wait_ticks)),
        ticks_to_us(EXT4_RESOURCE_LOCK_STATS.max_wait_ticks.load(Ordering::Relaxed)),
        ticks_to_us(DELTA_RESOURCE_LOCK_HOLD_TICKS.take(&EXT4_RESOURCE_LOCK_STATS.hold_ticks)),
        ticks_to_us(EXT4_RESOURCE_LOCK_STATS.max_hold_ticks.load(Ordering::Relaxed)),
    );
    emit_resource_lock_class_interval_deltas();
    emit_resource_lock_io_interval_deltas();
    println!(
        "[perf] interval_ext4_resource_registry lookups={} creates={} total_us={} max_us={}",
        DELTA_RESOURCE_REGISTRY_LOOKUPS.take(&EXT4_RESOURCE_LOCK_STATS.registry_lookups),
        DELTA_RESOURCE_REGISTRY_CREATES.take(&EXT4_RESOURCE_LOCK_STATS.registry_creates),
        ticks_to_us(DELTA_RESOURCE_REGISTRY_TICKS.take(&EXT4_RESOURCE_LOCK_STATS.registry_ticks)),
        ticks_to_us(
            EXT4_RESOURCE_LOCK_STATS
                .max_registry_ticks
                .load(Ordering::Relaxed)
        ),
    );
}

#[cfg(feature = "perf")]
fn emit_ext4_storage_cumulative() {
    let bcache = lwext4_rust::perf::bcache_perf_stats();
    println!(
        "[perf] ext4_bcache get_ops={} cache_hits={} cache_misses={} allocations={} allocation_races={} loader_ops={} loader_successes={} loader_errors={} wait_ops={} wait_rechecks={} wake_calls={} shake_calls={} clean_evictions={} shake_full_dirty={} shake_full_pinned={} capacity_overflows={} dirty_capacity_reclaim_runs={} dirty_capacity_reclaimed_blocks={} dirty_capacity_reclaim_stalls={} writeback_ops={} writeback_successes={} writeback_errors={} writeback_waits={} drops={} initial_resident_blocks={} resident_blocks={} max_resident_blocks={}",
        bcache.get_ops,
        bcache.cache_hits,
        bcache.cache_misses,
        bcache.allocations,
        bcache.allocation_races,
        bcache.loader_ops,
        bcache.loader_successes,
        bcache.loader_errors,
        bcache.wait_ops,
        bcache.wait_rechecks,
        bcache.wake_calls,
        bcache.shake_calls,
        bcache.clean_evictions,
        bcache.shake_full_dirty,
        bcache.shake_full_pinned,
        bcache.capacity_overflows,
        bcache.dirty_capacity_reclaim_runs,
        bcache.dirty_capacity_reclaimed_blocks,
        bcache.dirty_capacity_reclaim_stalls,
        bcache.writeback_ops,
        bcache.writeback_successes,
        bcache.writeback_errors,
        bcache.writeback_waits,
        bcache.drops,
        bcache.initial_resident_blocks,
        bcache.resident_blocks,
        bcache.max_resident_blocks,
    );
    println!(
        "[perf] ext4_bcache_io read_submits={} read_completions={} read_blocks={} read_errors={} write_submits={} write_completions={} write_blocks={} write_errors={}",
        bcache.read_submits,
        bcache.read_completions,
        bcache.read_blocks,
        bcache.read_errors,
        bcache.write_submits,
        bcache.write_completions,
        bcache.write_blocks,
        bcache.write_errors,
    );
    println!(
        "[perf] ext4_block_device submits={} read_requests={} write_requests={} flush_requests={} completed={} contended={} queued={} max_queue_depth={} bytes={} errors={} wait_us={} max_wait_us={} service_us={} max_service_us={} aligned_requests={} unaligned_requests={} sequential_requests={} max_request_bytes={}",
        EXT4_BLOCK_DEVICE_STATS.submits.load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS
            .read_requests
            .load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS
            .write_requests
            .load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS
            .flush_requests
            .load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS.completed.load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS.contended.load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS.queued.load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS
            .max_queue_depth
            .load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS.bytes.load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS.errors.load(Ordering::Relaxed),
        ticks_to_us(EXT4_BLOCK_DEVICE_STATS.wait_ticks.load(Ordering::Relaxed)),
        ticks_to_us(EXT4_BLOCK_DEVICE_STATS.max_wait_ticks.load(Ordering::Relaxed)),
        ticks_to_us(
            EXT4_BLOCK_DEVICE_STATS
                .service_ticks
                .load(Ordering::Relaxed)
        ),
        ticks_to_us(
            EXT4_BLOCK_DEVICE_STATS
                .max_service_ticks
                .load(Ordering::Relaxed)
        ),
        EXT4_BLOCK_DEVICE_STATS
            .aligned_requests
            .load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS
            .unaligned_requests
            .load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS
            .sequential_requests
            .load(Ordering::Relaxed),
        EXT4_BLOCK_DEVICE_STATS
            .max_request_bytes
            .load(Ordering::Relaxed),
    );
    println!(
        "[perf] ext4_bcache_completion_wait samples={} wait_us={} max_wait_us={} lock_held_samples={} lock_held_wait_us={} unlocked_samples={} unlocked_wait_us={}",
        EXT4_BCACHE_WAIT_SAMPLES.load(Ordering::Relaxed),
        ticks_to_us(EXT4_BCACHE_WAIT_TICKS.load(Ordering::Relaxed)),
        ticks_to_us(EXT4_BCACHE_WAIT_MAX_TICKS.load(Ordering::Relaxed)),
        EXT4_BCACHE_LOCKED_WAIT.samples.load(Ordering::Relaxed),
        ticks_to_us(EXT4_BCACHE_LOCKED_WAIT.ticks.load(Ordering::Relaxed)),
        EXT4_BCACHE_UNLOCKED_WAIT.samples.load(Ordering::Relaxed),
        ticks_to_us(EXT4_BCACHE_UNLOCKED_WAIT.ticks.load(Ordering::Relaxed)),
    );
    println!(
        "[perf] ext4_resource_locks acquires={} contended={} queued={} max_queue_depth={} wait_us={} max_wait_us={} hold_us={} max_hold_us={}",
        EXT4_RESOURCE_LOCK_STATS.acquires.load(Ordering::Relaxed),
        EXT4_RESOURCE_LOCK_STATS.contended.load(Ordering::Relaxed),
        EXT4_RESOURCE_LOCK_STATS.queued.load(Ordering::Relaxed),
        EXT4_RESOURCE_LOCK_STATS
            .max_queue_depth
            .load(Ordering::Relaxed),
        ticks_to_us(EXT4_RESOURCE_LOCK_STATS.wait_ticks.load(Ordering::Relaxed)),
        ticks_to_us(EXT4_RESOURCE_LOCK_STATS.max_wait_ticks.load(Ordering::Relaxed)),
        ticks_to_us(EXT4_RESOURCE_LOCK_STATS.hold_ticks.load(Ordering::Relaxed)),
        ticks_to_us(EXT4_RESOURCE_LOCK_STATS.max_hold_ticks.load(Ordering::Relaxed)),
    );
    emit_resource_lock_class_cumulative();
    emit_resource_lock_io_cumulative();
    println!(
        "[perf] ext4_resource_registry lookups={} creates={} total_us={} max_us={}",
        EXT4_RESOURCE_LOCK_STATS
            .registry_lookups
            .load(Ordering::Relaxed),
        EXT4_RESOURCE_LOCK_STATS
            .registry_creates
            .load(Ordering::Relaxed),
        ticks_to_us(
            EXT4_RESOURCE_LOCK_STATS
                .registry_ticks
                .load(Ordering::Relaxed)
        ),
        ticks_to_us(
            EXT4_RESOURCE_LOCK_STATS
                .max_registry_ticks
                .load(Ordering::Relaxed)
        ),
    );
}

fn emit_interval_deltas(now: usize) {
    let previous = DELTA_REPORT_MS.swap(now, Ordering::Relaxed);
    let elapsed_ms = if previous == 0 {
        now
    } else {
        now.saturating_sub(previous)
    };
    println!("[perf] interval t={}ms elapsed_ms={}", now, elapsed_ms);

    emit_section("INTERVAL SCHEDULER");
    println!(
        "[perf] interval_scheduler_wakeup local_enqueues={} remote_enqueues={} remote_idle_notifications={} remote_ipi_sent={} remote_ipi_failed={}",
        DELTA_SCHED_LOCAL_ENQUEUES.take(&SCHEDULER_LOCAL_ENQUEUES),
        DELTA_SCHED_REMOTE_ENQUEUES.take(&SCHEDULER_REMOTE_ENQUEUES),
        DELTA_SCHED_REMOTE_IDLE_NOTIFICATIONS.take(&SCHEDULER_REMOTE_IDLE_NOTIFICATIONS),
        DELTA_SCHED_REMOTE_IPI_SENT.take(&SCHEDULER_REMOTE_IPI_SENT),
        DELTA_SCHED_REMOTE_IPI_FAILED.take(&SCHEDULER_REMOTE_IPI_FAILED),
    );
    emit_duration_delta(
        "scheduler_dispatch",
        &SCHEDULER_DISPATCH_SAMPLES,
        &SCHEDULER_DISPATCH_TICKS,
        &DELTA_SCHEDULER_DISPATCH,
    );

    emit_section("INTERVAL PIPE");
    println!(
        "[perf] interval_pipe_wakeup reader_calls={} reader_tasks={} reader_poll_tasks={} reader_wait_rechecks={} writer_calls={} writer_tasks={} writer_poll_tasks={} writer_wait_rechecks={}",
        DELTA_PIPE_READER_WAKE_CALLS.take(&PIPE_READER_WAKE_CALLS),
        DELTA_PIPE_READER_WAKE_TASKS.take(&PIPE_READER_WAKE_TASKS),
        DELTA_PIPE_READER_WAKE_POLL_TASKS.take(&PIPE_READER_WAKE_POLL_TASKS),
        DELTA_PIPE_READ_WAIT_RECHECKS.take(&PIPE_READ_WAIT_RECHECKS),
        DELTA_PIPE_WRITER_WAKE_CALLS.take(&PIPE_WRITER_WAKE_CALLS),
        DELTA_PIPE_WRITER_WAKE_TASKS.take(&PIPE_WRITER_WAKE_TASKS),
        DELTA_PIPE_WRITER_WAKE_POLL_TASKS.take(&PIPE_WRITER_WAKE_POLL_TASKS),
        DELTA_PIPE_WRITE_WAIT_RECHECKS.take(&PIPE_WRITE_WAIT_RECHECKS),
    );
    emit_duration_delta(
        "pipe_read_wait",
        &PIPE_READ_WAIT_SAMPLES,
        &PIPE_READ_WAIT_TICKS,
        &DELTA_PIPE_READ_WAIT,
    );

    emit_section("INTERVAL SYSCALL");
    emit_duration_delta(
        "syscall_read",
        &SYSCALL_READ_SAMPLES,
        &SYSCALL_READ_TICKS,
        &DELTA_SYSCALL_READ,
    );
    emit_duration_delta(
        "syscall_write",
        &SYSCALL_WRITE_SAMPLES,
        &SYSCALL_WRITE_TICKS,
        &DELTA_SYSCALL_WRITE,
    );
    emit_duration_delta(
        "syscall_open",
        &SYSCALL_OPEN_SAMPLES,
        &SYSCALL_OPEN_TICKS,
        &DELTA_SYSCALL_OPEN,
    );
    emit_duration_delta(
        "syscall_stat",
        &SYSCALL_STAT_SAMPLES,
        &SYSCALL_STAT_TICKS,
        &DELTA_SYSCALL_STAT,
    );
    emit_duration_delta(
        "syscall_path",
        &SYSCALL_PATH_SAMPLES,
        &SYSCALL_PATH_TICKS,
        &DELTA_SYSCALL_PATH,
    );

    emit_section("INTERVAL FS");
    emit_raw_phase_delta(
        "write_open",
        &EXT4_WRITE_OPEN_SAMPLES,
        &EXT4_WRITE_OPEN_TICKS,
        &EXT4_WRITE_OPEN_MAX_TICKS,
        &DELTA_EXT4_WRITE_OPEN,
    );
    emit_raw_phase_delta(
        "write_quota",
        &EXT4_WRITE_QUOTA_SAMPLES,
        &EXT4_WRITE_QUOTA_TICKS,
        &EXT4_WRITE_QUOTA_MAX_TICKS,
        &DELTA_EXT4_WRITE_QUOTA,
    );
    emit_raw_phase_delta(
        "write_data",
        &EXT4_WRITE_DATA_SAMPLES,
        &EXT4_WRITE_DATA_TICKS,
        &EXT4_WRITE_DATA_MAX_TICKS,
        &DELTA_EXT4_WRITE_DATA,
    );
    emit_phase_delta(
        "fstat_actual",
        &EXT4_FSTAT_ACTUAL_EXT4_FSTAT,
        &DELTA_EXT4_FSTAT_ACTUAL,
    );
    emit_phase_delta(
        "fstat_inner_cold_inode",
        &EXT4_FSTAT_INNER_MISSES.cold_inode,
        &DELTA_EXT4_FSTAT_INNER_COLD,
    );
    emit_phase_delta(
        "fstat_inner_dense_write_back",
        &EXT4_FSTAT_INNER_MISSES.dense_write_back,
        &DELTA_EXT4_FSTAT_INNER_DENSE,
    );
    emit_phase_delta(
        "fstat_inner_sparse_buffered_write",
        &EXT4_FSTAT_INNER_MISSES.sparse_buffered_write,
        &DELTA_EXT4_FSTAT_INNER_SPARSE,
    );
    emit_phase_delta(
        "rename_write_back_cache",
        &EXT4_RENAME_WRITE_BACK_CACHE,
        &DELTA_EXT4_RENAME_WRITE_BACK,
    );
    emit_phase_delta(
        "rename_dense_write_back",
        &EXT4_RENAME_DENSE_WRITE_BACK,
        &DELTA_EXT4_RENAME_DENSE,
    );
    emit_phase_delta(
        "namespace_create",
        &EXT4_NAMESPACE_CREATE,
        &DELTA_EXT4_NAMESPACE_CREATE,
    );
    emit_phase_delta(
        "namespace_unlink",
        &EXT4_NAMESPACE_UNLINK,
        &DELTA_EXT4_NAMESPACE_UNLINK,
    );
    #[cfg(feature = "perf")]
    emit_write_cache_interval_deltas();
    #[cfg(feature = "perf")]
    {
        emit_section("INTERVAL EXT4 STORAGE");
        emit_ext4_storage_interval_deltas();
    }
}

pub(super) fn emit_report(now: usize) {
    emit_section("FS");
    println!(
        "[perf] ext4 reads={} bytes={} byte_cache_read_hits={} byte_cache_read_hit_bytes={} file_cache hit={} miss={} page_faults={} readahead_ops={} readahead_pages={} readahead_bytes={}",
        EXT4_READ_OPS.load(Ordering::Relaxed),
        EXT4_READ_BYTES.load(Ordering::Relaxed),
        EXT4_BYTE_CACHE_READ_HITS.load(Ordering::Relaxed),
        EXT4_BYTE_CACHE_READ_HIT_BYTES.load(Ordering::Relaxed),
        FILE_CACHE_HITS.load(Ordering::Relaxed),
        FILE_CACHE_MISSES.load(Ordering::Relaxed),
        FILE_PAGE_FAULTS.load(Ordering::Relaxed),
        FILE_CACHE_READAHEAD_OPS.load(Ordering::Relaxed),
        FILE_CACHE_READAHEAD_PAGES.load(Ordering::Relaxed),
        FILE_CACHE_READAHEAD_BYTES.load(Ordering::Relaxed),
    );
    println!(
        "[perf] file_cache_source mmap_hit={} mmap_miss={} read_page_hit={} read_page_miss={} splice_hit={} splice_miss={} read_bypass_request_ops={} read_bypass_request_bytes={} read_bypass_file_ops={} read_bypass_file_bytes={} read_bypass_nonregular_ops={} read_bypass_nonregular_bytes={} load_attempts={} load_races={}",
        FILE_CACHE_MMAP_HITS.load(Ordering::Relaxed),
        FILE_CACHE_MMAP_MISSES.load(Ordering::Relaxed),
        FILE_CACHE_READ_HITS.load(Ordering::Relaxed),
        FILE_CACHE_READ_MISSES.load(Ordering::Relaxed),
        FILE_CACHE_SPLICE_HITS.load(Ordering::Relaxed),
        FILE_CACHE_SPLICE_MISSES.load(Ordering::Relaxed),
        FILE_CACHE_READ_BYPASS_REQUEST_OPS.load(Ordering::Relaxed),
        FILE_CACHE_READ_BYPASS_REQUEST_BYTES.load(Ordering::Relaxed),
        FILE_CACHE_READ_BYPASS_FILE_OPS.load(Ordering::Relaxed),
        FILE_CACHE_READ_BYPASS_FILE_BYTES.load(Ordering::Relaxed),
        FILE_CACHE_READ_BYPASS_NONREGULAR_OPS.load(Ordering::Relaxed),
        FILE_CACHE_READ_BYPASS_NONREGULAR_BYTES.load(Ordering::Relaxed),
        FILE_CACHE_LOAD_ATTEMPTS.load(Ordering::Relaxed),
        FILE_CACHE_LOAD_RACES.load(Ordering::Relaxed),
    );
    println!(
        "[perf] file_cache_capacity resident_pages={} max_pages={} capacity_bypass_pages={} evictions={} eviction_scans={} eviction_second_chances={} eviction_dirty_skips={} eviction_in_use_skips={} eviction_deferred_retry_pages={} eviction_cooldown_bypasses={}",
        FILE_PAGE_CACHE.cached_page_count(),
        FILE_PAGE_CACHE.max_cached_pages(),
        FILE_CACHE_CAPACITY_BYPASS_PAGES.load(Ordering::Relaxed),
        FILE_CACHE_EVICTIONS.load(Ordering::Relaxed),
        FILE_CACHE_EVICTION_SCANS.load(Ordering::Relaxed),
        FILE_CACHE_EVICTION_SECOND_CHANCES.load(Ordering::Relaxed),
        FILE_CACHE_EVICTION_DIRTY_SKIPS.load(Ordering::Relaxed),
        FILE_CACHE_EVICTION_IN_USE_SKIPS.load(Ordering::Relaxed),
        FILE_CACHE_EVICTION_DEFERRED_RETRY_PAGES.load(Ordering::Relaxed),
        FILE_CACHE_EVICTION_COOLDOWN_BYPASSES.load(Ordering::Relaxed),
    );
    println!(
        "[perf] inode_read_source mmap_cache_fill_ops={} mmap_cache_fill_bytes={} mmap_demand_ops={} mmap_demand_bytes={} mmap_prefetch_ops={} mmap_prefetch_bytes={} page_cached_cold_run_ops={} page_cached_cold_run_bytes={} direct_bypass_ops={} direct_bypass_bytes={} other_ops={} other_bytes={}",
        INODE_READ_MMAP_CACHE_FILL.ops.load(Ordering::Relaxed),
        INODE_READ_MMAP_CACHE_FILL.bytes.load(Ordering::Relaxed),
        INODE_READ_MMAP_DEMAND.ops.load(Ordering::Relaxed),
        INODE_READ_MMAP_DEMAND.bytes.load(Ordering::Relaxed),
        INODE_READ_MMAP_PREFETCH.ops.load(Ordering::Relaxed),
        INODE_READ_MMAP_PREFETCH.bytes.load(Ordering::Relaxed),
        INODE_READ_PAGE_CACHED_COLD_RUN.ops.load(Ordering::Relaxed),
        INODE_READ_PAGE_CACHED_COLD_RUN.bytes.load(Ordering::Relaxed),
        INODE_READ_DIRECT_BYPASS.ops.load(Ordering::Relaxed),
        INODE_READ_DIRECT_BYPASS.bytes.load(Ordering::Relaxed),
        INODE_READ_OTHER.ops.load(Ordering::Relaxed),
        INODE_READ_OTHER.bytes.load(Ordering::Relaxed),
    );
    println!(
        "[perf] vfs_lookup fsidx_hit={} fsidx_miss={} path_index_hit={} dentry_positive_hit={} dentry_negative_hit={} dentry_miss={} dentry_positive_insert={} dentry_negative_insert={} dentry_invalidates={} dentry_invalidate_hits={} dentry_clear_calls={} dentry_parent_miss={} dentry_lookup_bypass_flags={} cached_parent_find={} root_find={} preserve_final_cache_hit={} fsidx_reclaimed={} fsidx_rebuilds={} fsidx_identity_epoch_hit={} fsidx_identity_live_probe={} fsidx_identity_stale_replace={} dentry_cleared_by_fsidx={} dentry_capacity_evictions={} dentry_capacity_evicted_entries={}",
        VFS_FSINDEX_HITS.load(Ordering::Relaxed),
        VFS_FSINDEX_MISSES.load(Ordering::Relaxed),
        VFS_PATH_INDEX_HITS.load(Ordering::Relaxed),
        VFS_DENTRY_POSITIVE_HITS.load(Ordering::Relaxed),
        VFS_DENTRY_NEGATIVE_HITS.load(Ordering::Relaxed),
        VFS_DENTRY_MISSES.load(Ordering::Relaxed),
        VFS_DENTRY_POSITIVE_INSERTS.load(Ordering::Relaxed),
        VFS_DENTRY_NEGATIVE_INSERTS.load(Ordering::Relaxed),
        VFS_DENTRY_INVALIDATES.load(Ordering::Relaxed),
        VFS_DENTRY_INVALIDATE_HITS.load(Ordering::Relaxed),
        VFS_DENTRY_CLEAR_CALLS.load(Ordering::Relaxed),
        VFS_DENTRY_PARENT_MISSES.load(Ordering::Relaxed),
        VFS_DENTRY_LOOKUP_BYPASS_FLAGS.load(Ordering::Relaxed),
        VFS_CACHED_PARENT_FINDS.load(Ordering::Relaxed),
        VFS_ROOT_FINDS.load(Ordering::Relaxed),
        VFS_PRESERVE_FINAL_CACHE_HITS.load(Ordering::Relaxed),
        VFS_FSINDEX_RECLAIMED.load(Ordering::Relaxed),
        VFS_FSINDEX_REBUILDS.load(Ordering::Relaxed),
        VFS_FSINDEX_IDENTITY_EPOCH_HITS.load(Ordering::Relaxed),
        VFS_FSINDEX_IDENTITY_LIVE_PROBES.load(Ordering::Relaxed),
        VFS_FSINDEX_IDENTITY_STALE_REPLACES.load(Ordering::Relaxed),
        VFS_DENTRY_CLEARED_BY_FSINDEX.load(Ordering::Relaxed),
        VFS_DENTRY_CAPACITY_EVICTIONS.load(Ordering::Relaxed),
        VFS_DENTRY_CAPACITY_EVICTED_ENTRIES.load(Ordering::Relaxed),
    );
    print!("[perf] ext4_fstat_path ");
    emit_ext4_phase_stats("fast_cached", &EXT4_FSTAT_FAST_CACHED);
    print!("[perf] ext4_fstat_path ");
    emit_ext4_phase_stats("directory_epoch_cached", &EXT4_FSTAT_DIRECTORY_EPOCH_CACHED);
    print!("[perf] ext4_fstat_path ");
    emit_ext4_phase_stats("post_wait_cached", &EXT4_FSTAT_POST_WAIT_CACHED);
    print!("[perf] ext4_fstat_path ");
    emit_ext4_phase_stats(
        "post_wait_directory_cached",
        &EXT4_FSTAT_POST_WAIT_DIRECTORY_CACHED,
    );
    print!("[perf] ext4_fstat_path ");
    emit_ext4_phase_stats("actual_ext4_fstat", &EXT4_FSTAT_ACTUAL_EXT4_FSTAT);
    print!("[perf] ext4_fstat_path ");
    emit_ext4_phase_stats("recover_live_path", &EXT4_FSTAT_RECOVER_LIVE_PATH);
    print!("[perf] ext4_fstat_stage ");
    emit_ext4_phase_stats("flush_sparse_write_buffer", &EXT4_FSTAT_SPARSE_WRITE_FLUSH);
    print!("[perf] ext4_fstat_stage ");
    emit_ext4_phase_stats("ext4_stat_get", &EXT4_FSTAT_STAT_GET);
    print!("[perf] ext4_fstat_stage ");
    emit_ext4_phase_stats("write_back_fallback", &EXT4_FSTAT_WRITE_BACK_FALLBACK);
    print!("[perf] ext4_fstat_stage ");
    emit_ext4_phase_stats("write_back_overlay", &EXT4_FSTAT_WRITE_BACK_OVERLAY);
    println!(
        "[perf] ext4_fstat_cache_invalidate cold_inode={} dense_write_back={} direct_write={} sparse_buffered_write={} truncate={} rename={} unlink={} hard_link={} metadata={} alias_recovery={} fsidx_rebuild={}",
        EXT4_FSTAT_CACHE_INVALIDATIONS
            .cold_inode
            .load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS
            .dense_write_back
            .load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS
            .direct_write
            .load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS
            .sparse_buffered_write
            .load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS.truncate.load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS.rename.load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS.unlink.load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS.hard_link.load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS.metadata.load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS
            .alias_recovery
            .load(Ordering::Relaxed),
        EXT4_FSTAT_CACHE_INVALIDATIONS
            .fsidx_rebuild
            .load(Ordering::Relaxed),
    );
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats("cold_inode", &EXT4_FSTAT_INNER_MISSES.cold_inode);
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats(
        "dense_write_back",
        &EXT4_FSTAT_INNER_MISSES.dense_write_back,
    );
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats("direct_write", &EXT4_FSTAT_INNER_MISSES.direct_write);
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats(
        "sparse_buffered_write",
        &EXT4_FSTAT_INNER_MISSES.sparse_buffered_write,
    );
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats("truncate", &EXT4_FSTAT_INNER_MISSES.truncate);
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats("rename", &EXT4_FSTAT_INNER_MISSES.rename);
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats("unlink", &EXT4_FSTAT_INNER_MISSES.unlink);
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats("hard_link", &EXT4_FSTAT_INNER_MISSES.hard_link);
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats("metadata", &EXT4_FSTAT_INNER_MISSES.metadata);
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats("alias_recovery", &EXT4_FSTAT_INNER_MISSES.alias_recovery);
    print!("[perf] ext4_fstat_inner_duration ");
    emit_ext4_phase_stats("fsidx_rebuild", &EXT4_FSTAT_INNER_MISSES.fsidx_rebuild);
    println!(
        "[perf] ext4_fstat_cold_inode regular_lookup_stat={} regular_no_lookup_stat={} directory_lookup_stat={} directory_no_lookup_stat={} special_lookup_stat={} special_no_lookup_stat={}",
        EXT4_FSTAT_COLD_INODE_COUNTS
            .regular_lookup_stat
            .load(Ordering::Relaxed),
        EXT4_FSTAT_COLD_INODE_COUNTS
            .regular_no_lookup_stat
            .load(Ordering::Relaxed),
        EXT4_FSTAT_COLD_INODE_COUNTS
            .directory_lookup_stat
            .load(Ordering::Relaxed),
        EXT4_FSTAT_COLD_INODE_COUNTS
            .directory_no_lookup_stat
            .load(Ordering::Relaxed),
        EXT4_FSTAT_COLD_INODE_COUNTS
            .special_lookup_stat
            .load(Ordering::Relaxed),
        EXT4_FSTAT_COLD_INODE_COUNTS
            .special_no_lookup_stat
            .load(Ordering::Relaxed),
    );
    println!(
        "[perf] ext4_fstat_directory_stat epoch_miss={} local_epoch_miss={} global_epoch_miss={}",
        EXT4_FSTAT_DIRECTORY_STAT_EPOCH_MISSES.load(Ordering::Relaxed),
        EXT4_FSTAT_DIRECTORY_STAT_LOCAL_EPOCH_MISSES.load(Ordering::Relaxed),
        EXT4_FSTAT_DIRECTORY_STAT_GLOBAL_EPOCH_MISSES.load(Ordering::Relaxed),
    );
    println!(
        "[perf] ext4_fstat_directory_parent local_updates={} global_fallbacks={}",
        EXT4_FSTAT_DIRECTORY_PARENT_LOCAL_UPDATES.load(Ordering::Relaxed),
        EXT4_FSTAT_DIRECTORY_PARENT_GLOBAL_FALLBACKS.load(Ordering::Relaxed),
    );
    print!("[perf] ext4_write_duration ");
    emit_duration(
        "open",
        &EXT4_WRITE_OPEN_SAMPLES,
        &EXT4_WRITE_OPEN_TICKS,
        &EXT4_WRITE_OPEN_MAX_TICKS,
    );
    print!("[perf] ext4_write_duration ");
    emit_duration(
        "quota",
        &EXT4_WRITE_QUOTA_SAMPLES,
        &EXT4_WRITE_QUOTA_TICKS,
        &EXT4_WRITE_QUOTA_MAX_TICKS,
    );
    print!("[perf] ext4_write_duration ");
    emit_duration(
        "data",
        &EXT4_WRITE_DATA_SAMPLES,
        &EXT4_WRITE_DATA_TICKS,
        &EXT4_WRITE_DATA_MAX_TICKS,
    );
    print!("[perf] ext4_rename_duration ");
    emit_ext4_phase_stats("write_back_cache", &EXT4_RENAME_WRITE_BACK_CACHE);
    print!("[perf] ext4_rename_write_back_duration ");
    emit_ext4_phase_stats("sparse_write_flush", &EXT4_RENAME_SPARSE_WRITE_FLUSH);
    print!("[perf] ext4_rename_write_back_duration ");
    emit_ext4_phase_stats("dense_write_back", &EXT4_RENAME_DENSE_WRITE_BACK);
    print!("[perf] ext4_rename_write_back_duration ");
    emit_ext4_phase_stats("path_cache_discard", &EXT4_RENAME_PATH_CACHE_DISCARD);
    print!("[perf] ext4_rename_duration ");
    emit_ext4_phase_stats("close", &EXT4_RENAME_CLOSE);
    print!("[perf] ext4_rename_duration ");
    emit_ext4_phase_stats("lwext4_rename", &EXT4_RENAME_LWEXT4_RENAME);
    print!("[perf] ext4_rename_duration ");
    emit_ext4_phase_stats("vfs_cache_invalidate", &EXT4_RENAME_VFS_CACHE_INVALIDATE);
    print!("[perf] ext4_namespace_duration ");
    emit_ext4_phase_stats("create", &EXT4_NAMESPACE_CREATE);
    print!("[perf] ext4_namespace_create_duration ");
    emit_ext4_phase_stats("exist_check", &EXT4_NAMESPACE_CREATE_EXIST_CHECK);
    print!("[perf] ext4_namespace_create_duration ");
    emit_ext4_phase_stats("dir_mk_or_file_open", &EXT4_NAMESPACE_CREATE_NODE_OPEN);
    print!("[perf] ext4_namespace_create_duration ");
    emit_ext4_phase_stats("file_close", &EXT4_NAMESPACE_CREATE_FILE_CLOSE);
    print!("[perf] ext4_namespace_create_duration ");
    emit_ext4_phase_stats("metadata_apply", &EXT4_NAMESPACE_CREATE_METADATA_APPLY);
    print!("[perf] ext4_namespace_create_duration ");
    emit_ext4_phase_stats("vfs_finish", &EXT4_NAMESPACE_CREATE_VFS_FINISH);
    print!("[perf] ext4_namespace_duration ");
    emit_ext4_phase_stats("unlink", &EXT4_NAMESPACE_UNLINK);
    print!("[perf] ext4_namespace_duration ");
    emit_ext4_phase_stats("truncate", &EXT4_NAMESPACE_TRUNCATE);
    print!("[perf] ext4_namespace_duration ");
    emit_ext4_phase_stats("link_symlink", &EXT4_NAMESPACE_LINK_SYMLINK);
    print!("[perf] ext4_metadata_duration ");
    emit_ext4_phase_stats("size", &EXT4_METADATA_SIZE);
    print!("[perf] ext4_metadata_duration ");
    emit_ext4_phase_stats("timestamp", &EXT4_METADATA_TIMESTAMP);
    print!("[perf] ext4_metadata_duration ");
    emit_ext4_phase_stats("mode", &EXT4_METADATA_MODE);
    print!("[perf] ext4_metadata_duration ");
    emit_ext4_phase_stats("owner", &EXT4_METADATA_OWNER);
    print!("[perf] ext4_metadata_duration ");
    emit_ext4_phase_stats("link_count", &EXT4_METADATA_LINK_COUNT);
    print!("[perf] ext4_metadata_duration ");
    emit_ext4_phase_stats("alias", &EXT4_METADATA_ALIAS);
    print!("[perf] ext4_metadata_duration ");
    emit_ext4_phase_stats("recovery", &EXT4_METADATA_RECOVERY);
    print!("[perf] ext4_metadata_duration ");
    emit_ext4_phase_stats("delay", &EXT4_METADATA_DELAY);
    print!("[perf] ext4_metadata_duration ");
    emit_ext4_phase_stats("read_all_prepare", &EXT4_METADATA_READ_ALL_PREPARE);
    #[cfg(feature = "perf")]
    {
        let write_cache = lwext4_rust::perf::write_back_cache_perf_stats();
        println!(
            "[perf] ext4_write_cache hit_ops={} hit_bytes={} fast_hit_ops={} fast_hit_bytes={} init_ops={} init_read_bytes={} evict_ops={} evict_writeback_bytes={} limit_flush_ops={} limit_flush_bytes={} direct_ops={} direct_bytes={} direct_disabled_ops={} direct_disabled_bytes={} direct_too_large_ops={} direct_too_large_bytes={} direct_uncached_ops={} direct_uncached_bytes={} direct_hole_ops={} direct_hole_bytes={} direct_limit_ops={} direct_limit_bytes={} sparse_buffer_ops={} sparse_buffer_bytes={} sparse_flush_ops={} sparse_flush_bytes={} sparse_flush_fstat_ops={} sparse_flush_fstat_bytes={} sparse_flush_close_ops={} sparse_flush_close_bytes={} sparse_flush_rename_ops={} sparse_flush_rename_bytes={} sparse_flush_truncate_ops={} sparse_flush_truncate_bytes={} sparse_flush_cache_evict_ops={} sparse_flush_cache_evict_bytes={} sparse_flush_other_ops={} sparse_flush_other_bytes={} sparse_read_overlay_ops={} sparse_read_overlay_bytes={} sparse_read_overlay_dirty_bytes={}",
            write_cache.cache_hit_ops,
            write_cache.cache_hit_bytes,
            write_cache.cache_fast_hit_ops,
            write_cache.cache_fast_hit_bytes,
            write_cache.cache_init_ops,
            write_cache.cache_init_read_bytes,
            write_cache.cache_evict_ops,
            write_cache.cache_evict_writeback_bytes,
            write_cache.cache_limit_flush_ops,
            write_cache.cache_limit_flush_bytes,
            write_cache.direct_write_ops,
            write_cache.direct_write_bytes,
            write_cache.direct_disabled_ops,
            write_cache.direct_disabled_bytes,
            write_cache.direct_too_large_ops,
            write_cache.direct_too_large_bytes,
            write_cache.direct_uncached_ops,
            write_cache.direct_uncached_bytes,
            write_cache.direct_hole_ops,
            write_cache.direct_hole_bytes,
            write_cache.direct_limit_ops,
            write_cache.direct_limit_bytes,
            write_cache.sparse_buffer_ops,
            write_cache.sparse_buffer_bytes,
            write_cache.sparse_flush_ops,
            write_cache.sparse_flush_bytes,
            write_cache.sparse_flush_fstat_ops,
            write_cache.sparse_flush_fstat_bytes,
            write_cache.sparse_flush_close_ops,
            write_cache.sparse_flush_close_bytes,
            write_cache.sparse_flush_rename_ops,
            write_cache.sparse_flush_rename_bytes,
            write_cache.sparse_flush_truncate_ops,
            write_cache.sparse_flush_truncate_bytes,
            write_cache.sparse_flush_cache_evict_ops,
            write_cache.sparse_flush_cache_evict_bytes,
            write_cache.sparse_flush_other_ops,
            write_cache.sparse_flush_other_bytes,
            write_cache.sparse_read_overlay_ops,
            write_cache.sparse_read_overlay_bytes,
            write_cache.sparse_read_overlay_dirty_bytes,
        );
        println!(
            "[perf] ext4_fstat_inner calls={} sparse_flush_batches={} sparse_flush_bytes={} stat_get_ops={} write_back_overlay_ops={} write_back_fallback_ops={}",
            write_cache.fstat_calls,
            write_cache.sparse_flush_fstat_ops,
            write_cache.sparse_flush_fstat_bytes,
            write_cache.fstat_stat_get_ops,
            write_cache.fstat_write_back_overlay_ops,
            write_cache.fstat_write_back_fallback_ops,
        );
        println!(
            "[perf] ext4_rename_write_back ops={} sparse_flush_bytes={} dense_write_back_bytes={} zero_byte_fast_path_ops={} path_cache_discard_ops={}",
            write_cache.rename_write_back_ops,
            write_cache.rename_sparse_flush_bytes,
            write_cache.rename_dense_write_back_bytes,
            write_cache.rename_zero_byte_fast_path_ops,
            write_cache.rename_path_cache_discard_ops,
        );
        println!(
            "[perf] ext4_sparse_buffer batches={} batch_runs={} batch_bytes={} batch_max_runs={} batch_max_bytes={} payload_limit_batches={} payload_limit_bytes={} run_limit_batches={} run_limit_bytes={} both_limits_batches={} both_limits_bytes={} allocation_failure_batches={} allocation_failure_bytes={} large_direct_batches={} large_direct_bytes={} global_budget_batches={} global_budget_bytes={} allocation_failure_ops={} allocation_failure_request_bytes={} large_direct_ops={} large_direct_request_bytes={} global_budget_direct_ops={} global_budget_direct_request_bytes={} resident_max_bytes={}",
            write_cache.sparse_flush_batches,
            write_cache.sparse_flush_batch_runs,
            write_cache.sparse_flush_batch_bytes,
            write_cache.sparse_flush_batch_max_runs,
            write_cache.sparse_flush_batch_max_bytes,
            write_cache.sparse_cache_evict_payload_limit_batches,
            write_cache.sparse_cache_evict_payload_limit_bytes,
            write_cache.sparse_cache_evict_run_limit_batches,
            write_cache.sparse_cache_evict_run_limit_bytes,
            write_cache.sparse_cache_evict_both_limits_batches,
            write_cache.sparse_cache_evict_both_limits_bytes,
            write_cache.sparse_cache_evict_allocation_failure_batches,
            write_cache.sparse_cache_evict_allocation_failure_bytes,
            write_cache.sparse_cache_evict_large_direct_batches,
            write_cache.sparse_cache_evict_large_direct_bytes,
            write_cache.sparse_cache_evict_global_budget_batches,
            write_cache.sparse_cache_evict_global_budget_bytes,
            write_cache.sparse_buffer_allocation_failure_ops,
            write_cache.sparse_buffer_allocation_failure_bytes,
            write_cache.sparse_large_direct_ops,
            write_cache.sparse_large_direct_bytes,
            write_cache.sparse_buffer_budget_direct_ops,
            write_cache.sparse_buffer_budget_direct_bytes,
            write_cache.sparse_buffer_resident_max_bytes,
        );
    }
    #[cfg(feature = "perf")]
    {
        emit_section("EXT4 STORAGE");
        emit_ext4_storage_cumulative();
    }
    emit_section("SCHEDULER");
    println!(
        "[perf] scheduler selections={} self_selections={} idle_loops={}",
        SCHEDULER_SELECTIONS.load(Ordering::Relaxed),
        SCHEDULER_SELF_SELECTIONS.load(Ordering::Relaxed),
        IDLE_LOOPS.load(Ordering::Relaxed),
    );
    let (scheduler_selections_by_hart, idle_loops_by_hart) = scheduler_hart_snapshot();
    let scheduler_ready_tasks = crate::task::ready_queue::ready_procs_num();
    let idle_published_by_hart = crate::task::idle_hart_snapshot();
    let remote_tlb_shootdowns_by_source = remote_tlb_shootdown_source_snapshot();
    println!(
        "[perf] scheduler_harts selections_by_hart={:?} idle_loops_by_hart={:?} ready_tasks={} idle_published_by_hart={:?}",
        scheduler_selections_by_hart,
        idle_loops_by_hart,
        scheduler_ready_tasks,
        idle_published_by_hart,
    );
    println!(
        "[perf] scheduler_wakeup local_enqueues={} remote_enqueues={} remote_idle_notifications={} remote_ipi_sent={} remote_ipi_failed={}",
        SCHEDULER_LOCAL_ENQUEUES.load(Ordering::Relaxed),
        SCHEDULER_REMOTE_ENQUEUES.load(Ordering::Relaxed),
        SCHEDULER_REMOTE_IDLE_NOTIFICATIONS.load(Ordering::Relaxed),
        SCHEDULER_REMOTE_IPI_SENT.load(Ordering::Relaxed),
        SCHEDULER_REMOTE_IPI_FAILED.load(Ordering::Relaxed),
    );
    print!("[perf] scheduler_duration ");
    emit_duration(
        "dispatch",
        &SCHEDULER_DISPATCH_SAMPLES,
        &SCHEDULER_DISPATCH_TICKS,
        &SCHEDULER_DISPATCH_MAX_TICKS,
    );
    emit_section("MM");
    println!(
        "[perf] remote_tlb shootdowns={} local_only={} remote={} target_harts={} acknowledgements={} page_fault={} cow={} munmap={} mprotect={} mremap={} fork_exec={} other={}",
        REMOTE_TLB_SHOOTDOWNS.load(Ordering::Relaxed),
        REMOTE_TLB_LOCAL_ONLY_SAMPLES.load(Ordering::Relaxed),
        REMOTE_TLB_REMOTE_SAMPLES.load(Ordering::Relaxed),
        REMOTE_TLB_TARGET_HARTS.load(Ordering::Relaxed),
        REMOTE_TLB_ACKNOWLEDGEMENTS.load(Ordering::Relaxed),
        remote_tlb_shootdowns_by_source[0],
        remote_tlb_shootdowns_by_source[1],
        remote_tlb_shootdowns_by_source[2],
        remote_tlb_shootdowns_by_source[3],
        remote_tlb_shootdowns_by_source[4],
        remote_tlb_shootdowns_by_source[5],
        remote_tlb_shootdowns_by_source[6],
    );
    println!(
        "[perf] cow_fault_resolution exclusive_upgrade={} shared_frame_copy={}",
        COW_EXCLUSIVE_UPGRADES.load(Ordering::Relaxed),
        COW_SHARED_FRAME_COPIES.load(Ordering::Relaxed),
    );
    emit_duration(
        "[perf] remote_tlb_local_only",
        &REMOTE_TLB_LOCAL_ONLY_SAMPLES,
        &REMOTE_TLB_LOCAL_ONLY_TICKS,
        &REMOTE_TLB_LOCAL_ONLY_MAX_TICKS,
    );
    emit_duration(
        "[perf] remote_tlb_remote",
        &REMOTE_TLB_REMOTE_SAMPLES,
        &REMOTE_TLB_REMOTE_TICKS,
        &REMOTE_TLB_REMOTE_MAX_TICKS,
    );
    emit_duration(
        "[perf] remote_tlb_mailbox_wait",
        &REMOTE_TLB_MAILBOX_WAIT_SAMPLES,
        &REMOTE_TLB_MAILBOX_WAIT_TICKS,
        &REMOTE_TLB_MAILBOX_WAIT_MAX_TICKS,
    );
    emit_duration(
        "[perf] remote_tlb_ack_latency",
        &REMOTE_TLB_ACK_LATENCY_SAMPLES,
        &REMOTE_TLB_ACK_LATENCY_TICKS,
        &REMOTE_TLB_ACK_LATENCY_MAX_TICKS,
    );
    emit_section("PIPE");
    println!(
        "[perf] pipe_io read_calls={} read_completed={} read_requested_bytes={} read_bytes={} read_short_calls={} write_calls={} write_completed={} write_requested_bytes={} write_bytes={} write_short_calls={}",
        PIPE_READ_CALLS.load(Ordering::Relaxed),
        PIPE_READ_COMPLETED_CALLS.load(Ordering::Relaxed),
        PIPE_READ_REQUESTED_BYTES.load(Ordering::Relaxed),
        PIPE_READ_BYTES.load(Ordering::Relaxed),
        PIPE_READ_SHORT_CALLS.load(Ordering::Relaxed),
        PIPE_WRITE_CALLS.load(Ordering::Relaxed),
        PIPE_WRITE_COMPLETED_CALLS.load(Ordering::Relaxed),
        PIPE_WRITE_REQUESTED_BYTES.load(Ordering::Relaxed),
        PIPE_WRITE_BYTES.load(Ordering::Relaxed),
        PIPE_WRITE_SHORT_CALLS.load(Ordering::Relaxed),
    );
    println!(
        "[perf] pipe_wakeup reader_calls={} reader_tasks={} reader_poll_tasks={} reader_wait_rechecks={} writer_calls={} writer_tasks={} writer_poll_tasks={} writer_wait_rechecks={}",
        PIPE_READER_WAKE_CALLS.load(Ordering::Relaxed),
        PIPE_READER_WAKE_TASKS.load(Ordering::Relaxed),
        PIPE_READER_WAKE_POLL_TASKS.load(Ordering::Relaxed),
        PIPE_READ_WAIT_RECHECKS.load(Ordering::Relaxed),
        PIPE_WRITER_WAKE_CALLS.load(Ordering::Relaxed),
        PIPE_WRITER_WAKE_TASKS.load(Ordering::Relaxed),
        PIPE_WRITER_WAKE_POLL_TASKS.load(Ordering::Relaxed),
        PIPE_WRITE_WAIT_RECHECKS.load(Ordering::Relaxed),
    );
    print!("[perf] pipe_duration ");
    emit_duration(
        "read_wait",
        &PIPE_READ_WAIT_SAMPLES,
        &PIPE_READ_WAIT_TICKS,
        &PIPE_READ_WAIT_MAX_TICKS,
    );
    print!("[perf] pipe_duration ");
    emit_duration(
        "write_wait",
        &PIPE_WRITE_WAIT_SAMPLES,
        &PIPE_WRITE_WAIT_TICKS,
        &PIPE_WRITE_WAIT_MAX_TICKS,
    );
    print!("[perf] pipe_duration ");
    emit_duration(
        "read_copy",
        &PIPE_READ_COPY_SAMPLES,
        &PIPE_READ_COPY_TICKS,
        &PIPE_READ_COPY_MAX_TICKS,
    );
    print!("[perf] pipe_duration ");
    emit_duration(
        "write_copy",
        &PIPE_WRITE_COPY_SAMPLES,
        &PIPE_WRITE_COPY_TICKS,
        &PIPE_WRITE_COPY_MAX_TICKS,
    );
    print!("[perf] pipe_duration ");
    emit_duration(
        "read_pipebuf_gather",
        &PIPE_READ_PIPEBUF_GATHER_SAMPLES,
        &PIPE_READ_PIPEBUF_GATHER_TICKS,
        &PIPE_READ_PIPEBUF_GATHER_MAX_TICKS,
    );
    print!("[perf] pipe_duration ");
    emit_duration(
        "read_user_copy",
        &PIPE_READ_USER_COPY_SAMPLES,
        &PIPE_READ_USER_COPY_TICKS,
        &PIPE_READ_USER_COPY_MAX_TICKS,
    );
    print!("[perf] pipe_duration ");
    emit_duration(
        "write_user_extract",
        &PIPE_WRITE_USER_EXTRACT_SAMPLES,
        &PIPE_WRITE_USER_EXTRACT_TICKS,
        &PIPE_WRITE_USER_EXTRACT_MAX_TICKS,
    );
    print!("[perf] pipe_duration ");
    emit_duration(
        "write_pipebuf_copy",
        &PIPE_WRITE_PIPEBUF_COPY_SAMPLES,
        &PIPE_WRITE_PIPEBUF_COPY_TICKS,
        &PIPE_WRITE_PIPEBUF_COPY_MAX_TICKS,
    );
    emit_section("NET");
    print!("[perf] socket_duration ");
    emit_duration(
        "tcp_recv_active",
        &TCP_RECV_ACTIVE_SAMPLES,
        &TCP_RECV_ACTIVE_TICKS,
        &TCP_RECV_ACTIVE_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "connect",
        &SYSCALL_NET_CONNECT_SAMPLES,
        &SYSCALL_NET_CONNECT_TICKS,
        &SYSCALL_NET_CONNECT_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "accept",
        &SYSCALL_NET_ACCEPT_SAMPLES,
        &SYSCALL_NET_ACCEPT_TICKS,
        &SYSCALL_NET_ACCEPT_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "accept_active",
        &SYSCALL_NET_ACCEPT_ACTIVE_SAMPLES,
        &SYSCALL_NET_ACCEPT_ACTIVE_TICKS,
        &SYSCALL_NET_ACCEPT_ACTIVE_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "send",
        &SYSCALL_NET_SEND_SAMPLES,
        &SYSCALL_NET_SEND_TICKS,
        &SYSCALL_NET_SEND_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "recv",
        &SYSCALL_NET_RECV_SAMPLES,
        &SYSCALL_NET_RECV_TICKS,
        &SYSCALL_NET_RECV_MAX_TICKS,
    );
    emit_section("SYSCALL");
    println!(
        "[perf] t={}ms syscalls total={} read={} write={} open={} close={} stat={} lseek={} mm={} process={} futex={} sigaction={} yield={}",
        now,
        SYSCALL_TOTAL.load(Ordering::Relaxed),
        SYSCALL_READ.load(Ordering::Relaxed),
        SYSCALL_WRITE.load(Ordering::Relaxed),
        SYSCALL_OPEN.load(Ordering::Relaxed),
        SYSCALL_CLOSE.load(Ordering::Relaxed),
        SYSCALL_STAT.load(Ordering::Relaxed),
        SYSCALL_LSEEK.load(Ordering::Relaxed),
        SYSCALL_MM.load(Ordering::Relaxed),
        SYSCALL_PROCESS.load(Ordering::Relaxed),
        SYSCALL_FUTEX.load(Ordering::Relaxed),
        SYSCALL_SIGACTION.load(Ordering::Relaxed),
        SYSCALL_SCHED_YIELD.load(Ordering::Relaxed),
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "read",
        &SYSCALL_READ_SAMPLES,
        &SYSCALL_READ_TICKS,
        &SYSCALL_READ_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "read_active",
        &SYSCALL_READ_ACTIVE_SAMPLES,
        &SYSCALL_READ_ACTIVE_TICKS,
        &SYSCALL_READ_ACTIVE_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "write",
        &SYSCALL_WRITE_SAMPLES,
        &SYSCALL_WRITE_TICKS,
        &SYSCALL_WRITE_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "write_active",
        &SYSCALL_WRITE_ACTIVE_SAMPLES,
        &SYSCALL_WRITE_ACTIVE_TICKS,
        &SYSCALL_WRITE_ACTIVE_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "open",
        &SYSCALL_OPEN_SAMPLES,
        &SYSCALL_OPEN_TICKS,
        &SYSCALL_OPEN_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "close",
        &SYSCALL_CLOSE_SAMPLES,
        &SYSCALL_CLOSE_TICKS,
        &SYSCALL_CLOSE_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "stat",
        &SYSCALL_STAT_SAMPLES,
        &SYSCALL_STAT_TICKS,
        &SYSCALL_STAT_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "lseek",
        &SYSCALL_LSEEK_SAMPLES,
        &SYSCALL_LSEEK_TICKS,
        &SYSCALL_LSEEK_MAX_TICKS,
    );
    print!("[perf] lseek_duration ");
    emit_duration(
        "impl",
        &LSEEK_IMPL_SAMPLES,
        &LSEEK_IMPL_TICKS,
        &LSEEK_IMPL_MAX_TICKS,
    );
    print!("[perf] lseek_duration ");
    emit_duration(
        "type_check",
        &LSEEK_TYPE_CHECK_SAMPLES,
        &LSEEK_TYPE_CHECK_TICKS,
        &LSEEK_TYPE_CHECK_MAX_TICKS,
    );
    print!("[perf] lseek_duration ");
    emit_duration(
        "size",
        &LSEEK_SIZE_SAMPLES,
        &LSEEK_SIZE_TICKS,
        &LSEEK_SIZE_MAX_TICKS,
    );
    print!("[perf] lseek_duration ");
    emit_duration(
        "sparse",
        &LSEEK_SPARSE_SAMPLES,
        &LSEEK_SPARSE_TICKS,
        &LSEEK_SPARSE_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "path",
        &SYSCALL_PATH_SAMPLES,
        &SYSCALL_PATH_TICKS,
        &SYSCALL_PATH_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "mm",
        &SYSCALL_MM_SAMPLES,
        &SYSCALL_MM_TICKS,
        &SYSCALL_MM_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "futex",
        &SYSCALL_FUTEX_SAMPLES,
        &SYSCALL_FUTEX_TICKS,
        &SYSCALL_FUTEX_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "sigaction",
        &SYSCALL_SIGACTION_SAMPLES,
        &SYSCALL_SIGACTION_TICKS,
        &SYSCALL_SIGACTION_MAX_TICKS,
    );
    emit_section("TASK");
    print!("[perf] syscall_duration ");
    emit_duration(
        "clone_total",
        &SYSCALL_PROCESS_CLONE_TOTAL_SAMPLES,
        &SYSCALL_PROCESS_CLONE_TOTAL_TICKS,
        &SYSCALL_PROCESS_CLONE_TOTAL_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "execve",
        &SYSCALL_PROCESS_EXEC_SAMPLES,
        &SYSCALL_PROCESS_EXEC_TICKS,
        &SYSCALL_PROCESS_EXEC_MAX_TICKS,
    );
    // Keep the historical label while reporting only active poll intervals.
    print!("[perf] syscall_duration ");
    emit_duration(
        "wait",
        &SYSCALL_PROCESS_WAIT_ACTIVE_SAMPLES,
        &SYSCALL_PROCESS_WAIT_ACTIVE_TICKS,
        &SYSCALL_PROCESS_WAIT_ACTIVE_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "address_space",
        &CLONE_ADDRESS_SPACE_SAMPLES,
        &CLONE_ADDRESS_SPACE_TICKS,
        &CLONE_ADDRESS_SPACE_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "process_total",
        &CLONE_PROCESS_TOTAL_SAMPLES,
        &CLONE_PROCESS_TOTAL_TICKS,
        &CLONE_PROCESS_TOTAL_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "active",
        &CLONE_ACTIVE_SAMPLES,
        &CLONE_ACTIVE_TICKS,
        &CLONE_ACTIVE_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "vfork_wait",
        &CLONE_VFORK_WAIT_SAMPLES,
        &CLONE_VFORK_WAIT_TICKS,
        &CLONE_VFORK_WAIT_MAX_TICKS,
    );
    print!("[perf] vfork_duration ");
    emit_duration(
        "child_to_exec",
        &VFORK_CHILD_TO_EXEC_SAMPLES,
        &VFORK_CHILD_TO_EXEC_TICKS,
        &VFORK_CHILD_TO_EXEC_MAX_TICKS,
    );
    print!("[perf] vfork_duration ");
    emit_duration(
        "exec_to_parent_ready",
        &VFORK_EXEC_TO_PARENT_READY_SAMPLES,
        &VFORK_EXEC_TO_PARENT_READY_TICKS,
        &VFORK_EXEC_TO_PARENT_READY_MAX_TICKS,
    );
    print!("[perf] vfork_duration ");
    emit_duration(
        "child_to_exit",
        &VFORK_CHILD_TO_EXIT_SAMPLES,
        &VFORK_CHILD_TO_EXIT_TICKS,
        &VFORK_CHILD_TO_EXIT_MAX_TICKS,
    );
    print!("[perf] vfork_duration ");
    emit_duration(
        "parent_ready_to_resume",
        &VFORK_PARENT_READY_TO_RESUME_SAMPLES,
        &VFORK_PARENT_READY_TO_RESUME_TICKS,
        &VFORK_PARENT_READY_TO_RESUME_MAX_TICKS,
    );
    println!(
        "[perf] vfork_release exec={} exit={} signal={}",
        VFORK_RELEASE_EXEC.load(Ordering::Relaxed),
        VFORK_RELEASE_EXIT.load(Ordering::Relaxed),
        VFORK_RELEASE_SIGNAL.load(Ordering::Relaxed),
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "bootstrap",
        &CLONE_BOOTSTRAP_SAMPLES,
        &CLONE_BOOTSTRAP_TICKS,
        &CLONE_BOOTSTRAP_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "parent_state",
        &CLONE_PARENT_STATE_SAMPLES,
        &CLONE_PARENT_STATE_TICKS,
        &CLONE_PARENT_STATE_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "process_create",
        &CLONE_PROCESS_CREATE_SAMPLES,
        &CLONE_PROCESS_CREATE_TICKS,
        &CLONE_PROCESS_CREATE_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "task_setup",
        &CLONE_TASK_SETUP_SAMPLES,
        &CLONE_TASK_SETUP_TICKS,
        &CLONE_TASK_SETUP_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "procfs_register",
        &CLONE_PROCFS_REGISTER_SAMPLES,
        &CLONE_PROCFS_REGISTER_TICKS,
        &CLONE_PROCFS_REGISTER_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "publish",
        &CLONE_PUBLISH_SAMPLES,
        &CLONE_PUBLISH_TICKS,
        &CLONE_PUBLISH_MAX_TICKS,
    );
    print!("[perf] clone_duration ");
    emit_duration(
        "enqueue",
        &CLONE_ENQUEUE_SAMPLES,
        &CLONE_ENQUEUE_TICKS,
        &CLONE_ENQUEUE_MAX_TICKS,
    );
    print!("[perf] procfs_duration ");
    emit_duration(
        "materialize",
        &PROCFS_MATERIALIZE_SAMPLES,
        &PROCFS_MATERIALIZE_TICKS,
        &PROCFS_MATERIALIZE_MAX_TICKS,
    );
    print!("[perf] exec_duration ");
    emit_duration(
        "image",
        &EXEC_IMAGE_SAMPLES,
        &EXEC_IMAGE_TICKS,
        &EXEC_IMAGE_MAX_TICKS,
    );
    print!("[perf] exec_duration ");
    emit_duration(
        "from_elf",
        &EXEC_FROM_ELF_SAMPLES,
        &EXEC_FROM_ELF_TICKS,
        &EXEC_FROM_ELF_MAX_TICKS,
    );
    print!("[perf] exec_duration ");
    emit_duration(
        "stack",
        &EXEC_STACK_SAMPLES,
        &EXEC_STACK_TICKS,
        &EXEC_STACK_MAX_TICKS,
    );
    print!("[perf] exec_duration ");
    emit_duration(
        "commit",
        &EXEC_COMMIT_SAMPLES,
        &EXEC_COMMIT_TICKS,
        &EXEC_COMMIT_MAX_TICKS,
    );
    print!("[perf] exec_loader_duration ");
    emit_duration(
        "kernel_space",
        &EXEC_KERNEL_SPACE_SAMPLES,
        &EXEC_KERNEL_SPACE_TICKS,
        &EXEC_KERNEL_SPACE_MAX_TICKS,
    );
    print!("[perf] exec_loader_duration ");
    emit_duration(
        "interp",
        &EXEC_INTERP_SAMPLES,
        &EXEC_INTERP_TICKS,
        &EXEC_INTERP_MAX_TICKS,
    );
    print!("[perf] exec_loader_duration ");
    emit_duration(
        "interp_read",
        &EXEC_INTERP_READ_SAMPLES,
        &EXEC_INTERP_READ_TICKS,
        &EXEC_INTERP_READ_MAX_TICKS,
    );
    print!("[perf] exec_loader_duration ");
    emit_duration(
        "interp_map",
        &EXEC_INTERP_MAP_SAMPLES,
        &EXEC_INTERP_MAP_TICKS,
        &EXEC_INTERP_MAP_MAX_TICKS,
    );
    print!("[perf] exec_loader_duration ");
    emit_duration(
        "map_elf",
        &EXEC_MAP_ELF_SAMPLES,
        &EXEC_MAP_ELF_TICKS,
        &EXEC_MAP_ELF_MAX_TICKS,
    );
    emit_interval_deltas(now);
}
