//! Filesystem performance counters: lwext4, VFS/page cache, and pipe I/O.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::time::get_ticks;
use crate::fs::{Ext4OpGuard, Ext4OpLock};

use super::common::{add, emit_duration, record_duration, ticks_to_us, update_max};
use super::maybe_report;
use super::syscall::{
    LSEEK_IMPL_MAX_TICKS, LSEEK_IMPL_SAMPLES, LSEEK_IMPL_TICKS, LSEEK_SIZE_MAX_TICKS,
    LSEEK_SIZE_SAMPLES, LSEEK_SIZE_TICKS, LSEEK_SPARSE_MAX_TICKS, LSEEK_SPARSE_SAMPLES,
    LSEEK_SPARSE_TICKS, LSEEK_TYPE_CHECK_MAX_TICKS, LSEEK_TYPE_CHECK_SAMPLES,
    LSEEK_TYPE_CHECK_TICKS,
};

pub(crate) static PIPE_READ_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_COMPLETED_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_REQUESTED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_SHORT_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_COMPLETED_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_REQUESTED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_SHORT_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_WAIT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_WAIT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_WAIT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_WAIT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_WAIT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_WAIT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_COPY_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_COPY_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_COPY_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_COPY_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_COPY_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_COPY_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
// Split the large-message data path into the formerly indirect operations on
// each side of the pipe. The existing read_copy/write_copy buckets stay intact
// for comparisons with older logs. A zero gather/copy bucket after optimization
// proves that the corresponding temporary data Vec is no longer materialized.
pub(crate) static PIPE_READ_PIPEBUF_GATHER_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_PIPEBUF_GATHER_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_PIPEBUF_GATHER_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_USER_COPY_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_USER_COPY_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_USER_COPY_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_USER_EXTRACT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_USER_EXTRACT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_USER_EXTRACT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_PIPEBUF_COPY_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_PIPEBUF_COPY_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_PIPEBUF_COPY_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READ_WAIT_RECHECKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITE_WAIT_RECHECKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READER_WAKE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READER_WAKE_TASKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_READER_WAKE_POLL_TASKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITER_WAKE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITER_WAKE_TASKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PIPE_WRITER_WAKE_POLL_TASKS: AtomicUsize = AtomicUsize::new(0);
// Scheduler enqueue placement is kept separate from pipe-specific wakeups.
// It identifies remote blocked-task wakeups that otherwise wait for an idle
// hart's periodic timer interrupt.
pub(crate) static EXT4_READ_OPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_BYTE_CACHE_READ_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_BYTE_CACHE_READ_HIT_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Aggregate timing for one class of lwext4 operation.
///
/// This remains deliberately caller-free: BuildStorm has enough concurrent
/// filesystem traffic that per-path maps or per-operation logging would alter
/// the contention we are trying to measure.
pub(crate) struct Ext4LockStats {
    pub(crate) samples: AtomicUsize,
    pub(crate) wait_ticks: AtomicUsize,
    pub(crate) hold_ticks: AtomicUsize,
    pub(crate) max_wait_ticks: AtomicUsize,
    pub(crate) max_hold_ticks: AtomicUsize,
}

impl Ext4LockStats {
    const fn new() -> Self {
        Self {
            samples: AtomicUsize::new(0),
            wait_ticks: AtomicUsize::new(0),
            hold_ticks: AtomicUsize::new(0),
            max_wait_ticks: AtomicUsize::new(0),
            max_hold_ticks: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn record(&self, wait_ticks: usize, hold_ticks: usize) {
        add(&self.samples, 1);
        add(&self.wait_ticks, wait_ticks);
        add(&self.hold_ticks, hold_ticks);
        update_max(&self.max_wait_ticks, wait_ticks);
        update_max(&self.max_hold_ticks, hold_ticks);
    }
}

/// Aggregate duration for one named phase inside a serialized lwext4 path.
///
/// Phase counters intentionally remain process- and caller-free.  Their role
/// is to explain a long `EXT4_OP_LOCK` hold before changing its lock boundary.
pub(crate) struct Ext4PhaseStats {
    samples: AtomicUsize,
    ticks: AtomicUsize,
    max_ticks: AtomicUsize,
}

impl Ext4PhaseStats {
    const fn new() -> Self {
        Self {
            samples: AtomicUsize::new(0),
            ticks: AtomicUsize::new(0),
            max_ticks: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn record(&self, elapsed: usize) {
        record_duration(&self.samples, &self.ticks, &self.max_ticks, elapsed);
    }
}

pub(crate) static EXT4_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
/// Aggregate across both pieces of a split `Ext4Inode::read_at()` slow path.
/// `samples` therefore counts global-lock acquisitions, not logical reads.
pub(crate) static EXT4_READ_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_READ_OPEN_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_READ_DATA_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_FIND_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_FSTAT_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_WRITE_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_RENAME_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_CLOSE_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_READ_ALL_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_READ_DIR_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_PATH_RESOLVE_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_METADATA_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_NAMESPACE_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_SYNC_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_SEEK_LOCK_STATS: Ext4LockStats = Ext4LockStats::new();
pub(crate) static EXT4_WRITE_OPEN_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_WRITE_OPEN_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_WRITE_OPEN_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_WRITE_QUOTA_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_WRITE_QUOTA_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_WRITE_QUOTA_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_WRITE_DATA_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_WRITE_DATA_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_WRITE_DATA_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_RENAME_WRITE_BACK_CACHE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_RENAME_CLOSE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_RENAME_LWEXT4_RENAME: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_RENAME_VFS_CACHE_INVALIDATE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_CREATE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_UNLINK: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_TRUNCATE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_LINK_SYMLINK: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_METADATA_SIZE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_METADATA_TIMESTAMP: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_METADATA_MODE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_METADATA_OWNER: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_METADATA_LINK_COUNT: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_METADATA_ALIAS: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_METADATA_RECOVERY: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_METADATA_DELAY: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_METADATA_READ_ALL_PREPARE: Ext4PhaseStats = Ext4PhaseStats::new();

pub(crate) static FILE_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_MMAP_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_MMAP_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READ_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READ_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_SPLICE_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_SPLICE_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READ_BYPASS_REQUEST_OPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READ_BYPASS_REQUEST_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READ_BYPASS_FILE_OPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READ_BYPASS_FILE_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READ_BYPASS_NONREGULAR_OPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READ_BYPASS_NONREGULAR_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_LOAD_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_LOAD_RACES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_PAGE_FAULTS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READAHEAD_OPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READAHEAD_PAGES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READAHEAD_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_POSITIVE_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_NEGATIVE_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_CACHED_PARENT_FINDS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_ROOT_FINDS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_PRESERVE_FINAL_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_RECLAIMED: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_CLEARED_BY_FSINDEX: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_CAPACITY_EVICTIONS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_CAPACITY_EVICTED_ENTRIES: AtomicUsize = AtomicUsize::new(0);
pub fn record_pipe_read_call(requested: usize) {
    add(&PIPE_READ_CALLS, 1);
    add(&PIPE_READ_REQUESTED_BYTES, requested);
}

/// Record one pipe read that returned a byte count to its caller.
#[inline]
pub fn record_pipe_read_complete(requested: usize, actual: usize) {
    add(&PIPE_READ_COMPLETED_CALLS, 1);
    add(&PIPE_READ_BYTES, actual);
    if actual < requested {
        add(&PIPE_READ_SHORT_CALLS, 1);
    }
}

/// Record one attempted pipe write before it can block or fail.
#[inline]
pub fn record_pipe_write_call(requested: usize) {
    add(&PIPE_WRITE_CALLS, 1);
    add(&PIPE_WRITE_REQUESTED_BYTES, requested);
}

/// Record one pipe write that returned a byte count to its caller.
#[inline]
pub fn record_pipe_write_complete(requested: usize, actual: usize) {
    add(&PIPE_WRITE_COMPLETED_CALLS, 1);
    add(&PIPE_WRITE_BYTES, actual);
    if actual < requested {
        add(&PIPE_WRITE_SHORT_CALLS, 1);
    }
}

#[inline]
pub fn record_pipe_read_wait_duration(elapsed: usize) {
    record_duration(
        &PIPE_READ_WAIT_SAMPLES,
        &PIPE_READ_WAIT_TICKS,
        &PIPE_READ_WAIT_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_pipe_write_wait_duration(elapsed: usize) {
    record_duration(
        &PIPE_WRITE_WAIT_SAMPLES,
        &PIPE_WRITE_WAIT_TICKS,
        &PIPE_WRITE_WAIT_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_pipe_read_copy_duration(elapsed: usize) {
    record_duration(
        &PIPE_READ_COPY_SAMPLES,
        &PIPE_READ_COPY_TICKS,
        &PIPE_READ_COPY_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_pipe_write_copy_duration(elapsed: usize) {
    record_duration(
        &PIPE_WRITE_COPY_SAMPLES,
        &PIPE_WRITE_COPY_TICKS,
        &PIPE_WRITE_COPY_MAX_TICKS,
        elapsed,
    );
}

/// Record the temporary-gather step from PipeBuf fragments before a large
/// pipe read copies the bytes to a user buffer.
#[inline]
pub fn record_pipe_read_pipebuf_gather_duration(elapsed: usize) {
    record_duration(
        &PIPE_READ_PIPEBUF_GATHER_SAMPLES,
        &PIPE_READ_PIPEBUF_GATHER_TICKS,
        &PIPE_READ_PIPEBUF_GATHER_MAX_TICKS,
        elapsed,
    );
}

/// Record the large pipe-read copy to userspace. The source is a gathered Vec
/// before optimization and PipeBuf fragments directly afterwards.
#[inline]
pub fn record_pipe_read_user_copy_duration(elapsed: usize) {
    record_duration(
        &PIPE_READ_USER_COPY_SAMPLES,
        &PIPE_READ_USER_COPY_TICKS,
        &PIPE_READ_USER_COPY_MAX_TICKS,
        elapsed,
    );
}

/// Record extraction of a large pipe write from a segmented user buffer into
/// the final PipeBuf Vec.
#[inline]
pub fn record_pipe_write_user_extract_duration(elapsed: usize) {
    record_duration(
        &PIPE_WRITE_USER_EXTRACT_SAMPLES,
        &PIPE_WRITE_USER_EXTRACT_TICKS,
        &PIPE_WRITE_USER_EXTRACT_MAX_TICKS,
        elapsed,
    );
}

/// Record insertion of a large write's final Vec into PipeBuf. Before the
/// move-based path this also included an additional allocation/copy.
#[inline]
pub fn record_pipe_write_pipebuf_copy_duration(elapsed: usize) {
    record_duration(
        &PIPE_WRITE_PIPEBUF_COPY_SAMPLES,
        &PIPE_WRITE_PIPEBUF_COPY_TICKS,
        &PIPE_WRITE_PIPEBUF_COPY_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_pipe_read_wait_recheck() {
    add(&PIPE_READ_WAIT_RECHECKS, 1);
}

#[inline]
pub fn record_pipe_write_wait_recheck() {
    add(&PIPE_WRITE_WAIT_RECHECKS, 1);
}

/// Record a reader wakeup. `task_woken` and `poll_woken` are kept separate
/// because task wait queues and poll registrations have different wake rules.
#[inline]
pub fn record_pipe_reader_wakeup(task_woken: usize, poll_woken: usize) {
    add(&PIPE_READER_WAKE_CALLS, 1);
    add(&PIPE_READER_WAKE_TASKS, task_woken);
    add(&PIPE_READER_WAKE_POLL_TASKS, poll_woken);
}

/// Record a writer wakeup. See [`record_pipe_reader_wakeup`] for the count
/// meanings.
#[inline]
pub fn record_pipe_writer_wakeup(task_woken: usize, poll_woken: usize) {
    add(&PIPE_WRITER_WAKE_CALLS, 1);
    add(&PIPE_WRITER_WAKE_TASKS, task_woken);
    add(&PIPE_WRITER_WAKE_POLL_TASKS, poll_woken);
}

/// Record the scheduler placement after a task becomes runnable.  `ipi_sent`
/// is meaningful only when a remote target had already published itself idle.
pub fn record_lseek_type_check_duration(elapsed: usize) {
    record_duration(
        &LSEEK_TYPE_CHECK_SAMPLES,
        &LSEEK_TYPE_CHECK_TICKS,
        &LSEEK_TYPE_CHECK_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_lseek_size_duration(elapsed: usize) {
    record_duration(
        &LSEEK_SIZE_SAMPLES,
        &LSEEK_SIZE_TICKS,
        &LSEEK_SIZE_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_lseek_sparse_duration(elapsed: usize) {
    record_duration(
        &LSEEK_SPARSE_SAMPLES,
        &LSEEK_SPARSE_TICKS,
        &LSEEK_SPARSE_MAX_TICKS,
        elapsed,
    );
}

/// Scope guard for the complete VFS-level lseek implementation, excluding
/// syscall dispatch and fd-table lookup.
pub struct LseekDurationGuard {
    begin: usize,
}

impl LseekDurationGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for LseekDurationGuard {
    #[inline]
    fn drop(&mut self) {
        record_duration(
            &LSEEK_IMPL_SAMPLES,
            &LSEEK_IMPL_TICKS,
            &LSEEK_IMPL_MAX_TICKS,
            get_ticks().saturating_sub(self.begin),
        );
    }
}

#[inline]
pub(crate) fn emit_ext4_phase_stats(label: &str, stats: &Ext4PhaseStats) {
    emit_duration(label, &stats.samples, &stats.ticks, &stats.max_ticks);
}

pub(crate) fn emit_ext4_lock_stats(label: &str, stats: &Ext4LockStats) {
    println!(
        "[perf] {} samples={} wait_us={} hold_us={} max_wait_us={} max_hold_us={}",
        label,
        stats.samples.load(Ordering::Relaxed),
        ticks_to_us(stats.wait_ticks.load(Ordering::Relaxed)),
        ticks_to_us(stats.hold_ticks.load(Ordering::Relaxed)),
        ticks_to_us(stats.max_wait_ticks.load(Ordering::Relaxed)),
        ticks_to_us(stats.max_hold_ticks.load(Ordering::Relaxed)),
    );
}
pub fn record_ext4_read(bytes: usize) {
    add(&EXT4_READ_OPS, 1);
    add(&EXT4_READ_BYTES, bytes);
}

#[inline]
pub fn record_ext4_byte_cache_read_hit(bytes: usize) {
    add(&EXT4_BYTE_CACHE_READ_HITS, 1);
    add(&EXT4_BYTE_CACHE_READ_HIT_BYTES, bytes);
}

#[inline]
pub fn record_ext4_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_LOCK_STATS.record(wait_ticks, hold_ticks);
    let samples = EXT4_LOCK_STATS.samples.load(Ordering::Relaxed);
    if samples & 0x0fff == 0 {
        maybe_report();
    }
}

#[inline]
pub fn record_ext4_read_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_READ_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_read_open_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_READ_OPEN_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_read_data_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_READ_DATA_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_find_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_FIND_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_fstat_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_FSTAT_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_write_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_WRITE_LOCK_STATS.record(wait_ticks, hold_ticks);
}

/// Lightweight lock classes identify the most contended lwext4 entry points
/// without changing the filesystem's single global serialization boundary.
#[derive(Clone, Copy)]
enum Ext4LockClass {
    ReadOpen,
    ReadData,
    Find,
    Fstat,
    Write,
    Rename,
    Close,
    ReadAll,
    ReadDir,
    PathResolve,
    Metadata,
    Namespace,
    Sync,
    Seek,
}

/// Perf wrapper around the single lwext4 operation guard.  It owns all lock
/// classification and timing so `ext4_lw` itself only implements synchronization.
pub(crate) struct Ext4ProfiledOpGuard<'a> {
    guard: Ext4OpGuard<'a>,
    class: Ext4LockClass,
    wait_ticks: usize,
    acquired_at: usize,
}

impl Ext4OpLock {
    pub(crate) fn lock_for_read_open(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::ReadOpen)
    }

    pub(crate) fn lock_for_read_data(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::ReadData)
    }

    pub(crate) fn lock_for_find(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Find)
    }

    pub(crate) fn lock_for_fstat(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Fstat)
    }

    pub(crate) fn lock_for_write(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Write)
    }

    pub(crate) fn lock_for_rename(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Rename)
    }

    pub(crate) fn lock_for_close(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Close)
    }

    pub(crate) fn lock_for_read_all(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::ReadAll)
    }

    pub(crate) fn lock_for_read_dir(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::ReadDir)
    }

    pub(crate) fn lock_for_path_resolve(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::PathResolve)
    }

    pub(crate) fn lock_for_metadata(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Metadata)
    }

    pub(crate) fn lock_for_namespace(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Namespace)
    }

    pub(crate) fn lock_for_sync(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Sync)
    }

    pub(crate) fn lock_for_seek(&self) -> Ext4ProfiledOpGuard<'_> {
        self.lock_profiled(Ext4LockClass::Seek)
    }

    fn lock_profiled(&self, class: Ext4LockClass) -> Ext4ProfiledOpGuard<'_> {
        let wait_start = get_ticks();
        let guard = self.lock();
        let acquired_at = get_ticks();
        Ext4ProfiledOpGuard {
            guard,
            class,
            wait_ticks: acquired_at.saturating_sub(wait_start),
            acquired_at,
        }
    }
}

impl Drop for Ext4ProfiledOpGuard<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "perf")]
        {
            let released_at = get_ticks();
            if self.guard.release() {
                let hold_ticks = released_at.saturating_sub(self.acquired_at);
                match self.class {
                    Ext4LockClass::ReadOpen => {
                        record_ext4_read_lock(self.wait_ticks, hold_ticks);
                        record_ext4_read_open_lock(self.wait_ticks, hold_ticks);
                    }
                    Ext4LockClass::ReadData => {
                        record_ext4_read_lock(self.wait_ticks, hold_ticks);
                        record_ext4_read_data_lock(self.wait_ticks, hold_ticks);
                    }
                    Ext4LockClass::Find => record_ext4_find_lock(self.wait_ticks, hold_ticks),
                    Ext4LockClass::Fstat => record_ext4_fstat_lock(self.wait_ticks, hold_ticks),
                    Ext4LockClass::Write => record_ext4_write_lock(self.wait_ticks, hold_ticks),
                    Ext4LockClass::Rename => record_ext4_rename_lock(self.wait_ticks, hold_ticks),
                    Ext4LockClass::Close => record_ext4_close_lock(self.wait_ticks, hold_ticks),
                    Ext4LockClass::ReadAll => {
                        record_ext4_read_all_lock(self.wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::ReadDir => {
                        record_ext4_read_dir_lock(self.wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::PathResolve => {
                        record_ext4_path_resolve_lock(self.wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Metadata => {
                        record_ext4_metadata_lock(self.wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Namespace => {
                        record_ext4_namespace_lock(self.wait_ticks, hold_ticks)
                    }
                    Ext4LockClass::Sync => record_ext4_sync_lock(self.wait_ticks, hold_ticks),
                    Ext4LockClass::Seek => record_ext4_seek_lock(self.wait_ticks, hold_ticks),
                }
                record_ext4_lock(self.wait_ticks, hold_ticks);
            }
        }
    }
}

/// A mutually exclusive phase inside `Ext4Inode::write_at()`.  The outer
/// lwext4 operation guard already accounts for lock wait and whole-section
/// hold time; these buckets identify which work performed while holding it is
/// worth optimizing.
pub enum Ext4WritePhase {
    Open,
    Quota,
    Data,
}

/// Scope guard which includes early error returns in a write phase sample.
pub struct Ext4WritePhaseGuard {
    phase: Ext4WritePhase,
    begin: usize,
}

impl Ext4WritePhaseGuard {
    #[inline]
    pub fn new(phase: Ext4WritePhase) -> Self {
        Self {
            phase,
            begin: get_ticks(),
        }
    }
}

impl Drop for Ext4WritePhaseGuard {
    #[inline]
    fn drop(&mut self) {
        let elapsed = get_ticks().saturating_sub(self.begin);
        match self.phase {
            Ext4WritePhase::Open => record_duration(
                &EXT4_WRITE_OPEN_SAMPLES,
                &EXT4_WRITE_OPEN_TICKS,
                &EXT4_WRITE_OPEN_MAX_TICKS,
                elapsed,
            ),
            Ext4WritePhase::Quota => record_duration(
                &EXT4_WRITE_QUOTA_SAMPLES,
                &EXT4_WRITE_QUOTA_TICKS,
                &EXT4_WRITE_QUOTA_MAX_TICKS,
                elapsed,
            ),
            Ext4WritePhase::Data => record_duration(
                &EXT4_WRITE_DATA_SAMPLES,
                &EXT4_WRITE_DATA_TICKS,
                &EXT4_WRITE_DATA_MAX_TICKS,
                elapsed,
            ),
        }
    }
}

/// Mutually exclusive stages inside `Ext4Inode::rename()` while it holds the
/// mount-wide lwext4 gate.
#[derive(Clone, Copy)]
pub enum Ext4RenamePhase {
    WriteBackCache,
    Close,
    Lwext4Rename,
    VfsCacheInvalidate,
}

/// Namespace-changing operations which currently share the broad namespace
/// lock class.  The phase shows the real operation before any lock shortening
/// is considered.
#[derive(Clone, Copy)]
pub enum Ext4NamespacePhase {
    Create,
    Unlink,
    Truncate,
    LinkSymlink,
}

/// Metadata operations which currently share the broad metadata lock class.
/// `Recovery` is nested inside the failed mode/owner paths and therefore
/// identifies the alias scan and descriptor transition separately.
#[derive(Clone, Copy)]
pub enum Ext4MetadataPhase {
    Size,
    Timestamp,
    Mode,
    Owner,
    LinkCount,
    Alias,
    Recovery,
    Delay,
    ReadAllPrepare,
}

#[derive(Clone, Copy)]
enum Ext4InodePhase {
    Rename(Ext4RenamePhase),
    Namespace(Ext4NamespacePhase),
    Metadata(Ext4MetadataPhase),
}

/// Scope guard for a phase that executes while `EXT4_OP_LOCK` is held.  Drop
/// based accounting keeps failed lwext4 calls in the same aggregate as
/// successful ones without per-call logging.
pub struct Ext4InodePhaseGuard {
    phase: Ext4InodePhase,
    begin: usize,
}

impl Ext4InodePhaseGuard {
    #[inline]
    pub fn rename(phase: Ext4RenamePhase) -> Self {
        Self {
            phase: Ext4InodePhase::Rename(phase),
            begin: get_ticks(),
        }
    }

    #[inline]
    pub fn namespace(phase: Ext4NamespacePhase) -> Self {
        Self {
            phase: Ext4InodePhase::Namespace(phase),
            begin: get_ticks(),
        }
    }

    #[inline]
    pub fn metadata(phase: Ext4MetadataPhase) -> Self {
        Self {
            phase: Ext4InodePhase::Metadata(phase),
            begin: get_ticks(),
        }
    }
}

impl Drop for Ext4InodePhaseGuard {
    #[inline]
    fn drop(&mut self) {
        let elapsed = get_ticks().saturating_sub(self.begin);
        match self.phase {
            Ext4InodePhase::Rename(Ext4RenamePhase::WriteBackCache) => {
                EXT4_RENAME_WRITE_BACK_CACHE.record(elapsed)
            }
            Ext4InodePhase::Rename(Ext4RenamePhase::Close) => EXT4_RENAME_CLOSE.record(elapsed),
            Ext4InodePhase::Rename(Ext4RenamePhase::Lwext4Rename) => {
                EXT4_RENAME_LWEXT4_RENAME.record(elapsed)
            }
            Ext4InodePhase::Rename(Ext4RenamePhase::VfsCacheInvalidate) => {
                EXT4_RENAME_VFS_CACHE_INVALIDATE.record(elapsed)
            }
            Ext4InodePhase::Namespace(Ext4NamespacePhase::Create) => {
                EXT4_NAMESPACE_CREATE.record(elapsed)
            }
            Ext4InodePhase::Namespace(Ext4NamespacePhase::Unlink) => {
                EXT4_NAMESPACE_UNLINK.record(elapsed)
            }
            Ext4InodePhase::Namespace(Ext4NamespacePhase::Truncate) => {
                EXT4_NAMESPACE_TRUNCATE.record(elapsed)
            }
            Ext4InodePhase::Namespace(Ext4NamespacePhase::LinkSymlink) => {
                EXT4_NAMESPACE_LINK_SYMLINK.record(elapsed)
            }
            Ext4InodePhase::Metadata(Ext4MetadataPhase::Size) => EXT4_METADATA_SIZE.record(elapsed),
            Ext4InodePhase::Metadata(Ext4MetadataPhase::Timestamp) => {
                EXT4_METADATA_TIMESTAMP.record(elapsed)
            }
            Ext4InodePhase::Metadata(Ext4MetadataPhase::Mode) => EXT4_METADATA_MODE.record(elapsed),
            Ext4InodePhase::Metadata(Ext4MetadataPhase::Owner) => {
                EXT4_METADATA_OWNER.record(elapsed)
            }
            Ext4InodePhase::Metadata(Ext4MetadataPhase::LinkCount) => {
                EXT4_METADATA_LINK_COUNT.record(elapsed)
            }
            Ext4InodePhase::Metadata(Ext4MetadataPhase::Alias) => {
                EXT4_METADATA_ALIAS.record(elapsed)
            }
            Ext4InodePhase::Metadata(Ext4MetadataPhase::Recovery) => {
                EXT4_METADATA_RECOVERY.record(elapsed)
            }
            Ext4InodePhase::Metadata(Ext4MetadataPhase::Delay) => {
                EXT4_METADATA_DELAY.record(elapsed)
            }
            Ext4InodePhase::Metadata(Ext4MetadataPhase::ReadAllPrepare) => {
                EXT4_METADATA_READ_ALL_PREPARE.record(elapsed)
            }
        }
    }
}

#[inline]
pub fn record_ext4_rename_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_RENAME_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_close_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_CLOSE_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_read_all_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_READ_ALL_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_read_dir_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_READ_DIR_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_path_resolve_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_PATH_RESOLVE_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_metadata_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_METADATA_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_namespace_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_NAMESPACE_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_sync_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_SYNC_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_ext4_seek_lock(wait_ticks: usize, hold_ticks: usize) {
    EXT4_SEEK_LOCK_STATS.record(wait_ticks, hold_ticks);
}

#[inline]
pub fn record_file_cache_hit() {
    add(&FILE_CACHE_HITS, 1);
}

#[inline]
pub fn record_file_cache_miss() {
    add(&FILE_CACHE_MISSES, 1);
}

#[inline]
pub fn record_file_cache_mmap_hit() {
    add(&FILE_CACHE_MMAP_HITS, 1);
}

#[inline]
pub fn record_file_cache_mmap_miss() {
    add(&FILE_CACHE_MMAP_MISSES, 1);
}

#[inline]
pub fn record_file_cache_read_hit(pages: usize) {
    add(&FILE_CACHE_READ_HITS, pages);
}

#[inline]
pub fn record_file_cache_read_miss(pages: usize) {
    add(&FILE_CACHE_READ_MISSES, pages);
}

#[inline]
pub fn record_file_cache_splice_hit() {
    add(&FILE_CACHE_SPLICE_HITS, 1);
}

#[inline]
pub fn record_file_cache_splice_miss() {
    add(&FILE_CACHE_SPLICE_MISSES, 1);
}

#[inline]
pub fn record_file_cache_read_bypass_request_size(bytes: usize) {
    add(&FILE_CACHE_READ_BYPASS_REQUEST_OPS, 1);
    add(&FILE_CACHE_READ_BYPASS_REQUEST_BYTES, bytes);
}

#[inline]
pub fn record_file_cache_read_bypass_file_size(bytes: usize) {
    add(&FILE_CACHE_READ_BYPASS_FILE_OPS, 1);
    add(&FILE_CACHE_READ_BYPASS_FILE_BYTES, bytes);
}

#[inline]
pub fn record_file_cache_read_bypass_nonregular(bytes: usize) {
    add(&FILE_CACHE_READ_BYPASS_NONREGULAR_OPS, 1);
    add(&FILE_CACHE_READ_BYPASS_NONREGULAR_BYTES, bytes);
}

/// Count one page-cache miss that proceeds to a potentially serialized
/// filesystem read. A later `load_race` means another hart published the same
/// page while this task was in flight, so this attempt performed redundant I/O.
#[inline]
pub fn record_file_cache_load_attempt() {
    add(&FILE_CACHE_LOAD_ATTEMPTS, 1);
}

#[inline]
pub fn record_file_cache_load_race() {
    add(&FILE_CACHE_LOAD_RACES, 1);
}

#[inline]
pub fn record_file_page_fault() {
    add(&FILE_PAGE_FAULTS, 1);
}

#[inline]
pub fn record_file_cache_readahead(pages: usize, bytes: usize) {
    add(&FILE_CACHE_READAHEAD_OPS, 1);
    add(&FILE_CACHE_READAHEAD_PAGES, pages);
    add(&FILE_CACHE_READAHEAD_BYTES, bytes);
}

#[inline]
pub fn record_vfs_fsidx_hit() {
    add(&VFS_FSINDEX_HITS, 1);
}

#[inline]
pub fn record_vfs_fsidx_miss() {
    add(&VFS_FSINDEX_MISSES, 1);
}

#[inline]
pub fn record_vfs_dentry_positive_hit() {
    add(&VFS_DENTRY_POSITIVE_HITS, 1);
}

#[inline]
pub fn record_vfs_dentry_negative_hit() {
    add(&VFS_DENTRY_NEGATIVE_HITS, 1);
}

#[inline]
pub fn record_vfs_dentry_miss() {
    add(&VFS_DENTRY_MISSES, 1);
}

#[inline]
pub fn record_vfs_cached_parent_find() {
    add(&VFS_CACHED_PARENT_FINDS, 1);
}

#[inline]
pub fn record_vfs_root_find() {
    add(&VFS_ROOT_FINDS, 1);
}

#[inline]
pub fn record_vfs_preserve_final_cache_hit() {
    add(&VFS_PRESERVE_FINAL_CACHE_HITS, 1);
}

#[inline]
pub fn record_vfs_fsidx_reclaim(reclaimed_inodes: usize, cleared_dentries: usize) {
    add(&VFS_FSINDEX_RECLAIMED, reclaimed_inodes);
    add(&VFS_DENTRY_CLEARED_BY_FSINDEX, cleared_dentries);
}

#[inline]
pub fn record_vfs_dentry_capacity_evict(entries: usize) {
    add(&VFS_DENTRY_CAPACITY_EVICTIONS, 1);
    add(&VFS_DENTRY_CAPACITY_EVICTED_ENTRIES, entries);
}
