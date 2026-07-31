//! Lightweight aggregate instrumentation owned by the lwext4 wrapper.
//!
//! This crate cannot depend on Ya2yOS's kernel tick source.  It therefore
//! keeps only relaxed operation/byte counters here; `os::utils::perf` reads a
//! snapshot and supplies lock and phase timing around the wrapper calls.

#[cfg(feature = "perf")]
use core::sync::atomic::{AtomicUsize, Ordering};

/// Aggregate counters for the whole-file write-back cache and the fstat
/// sub-operations performed by `Ext4File`.
#[cfg(feature = "perf")]
#[derive(Clone, Copy, Debug, Default)]
pub struct WriteBackCachePerfStats {
    pub cache_hit_ops: usize,
    pub cache_hit_bytes: usize,
    pub cache_fast_hit_ops: usize,
    pub cache_fast_hit_bytes: usize,
    pub cache_init_ops: usize,
    pub cache_init_read_bytes: usize,
    pub cache_evict_ops: usize,
    pub cache_evict_writeback_bytes: usize,
    pub cache_limit_flush_ops: usize,
    pub cache_limit_flush_bytes: usize,
    pub direct_write_ops: usize,
    pub direct_write_bytes: usize,
    pub direct_disabled_ops: usize,
    pub direct_disabled_bytes: usize,
    pub direct_too_large_ops: usize,
    pub direct_too_large_bytes: usize,
    pub direct_uncached_ops: usize,
    pub direct_uncached_bytes: usize,
    pub direct_hole_ops: usize,
    pub direct_hole_bytes: usize,
    pub direct_limit_ops: usize,
    pub direct_limit_bytes: usize,
    pub sparse_buffer_ops: usize,
    pub sparse_buffer_bytes: usize,
    /// Sparse `ext4_fwrite` runs, not logical flush batches.
    pub sparse_flush_ops: usize,
    pub sparse_flush_bytes: usize,
    /// Non-empty sparse-buffer flush batches and their committed bytes,
    /// grouped by the barrier that required publication.
    pub sparse_flush_fstat_ops: usize,
    pub sparse_flush_fstat_bytes: usize,
    pub sparse_flush_close_ops: usize,
    pub sparse_flush_close_bytes: usize,
    pub sparse_flush_rename_ops: usize,
    pub sparse_flush_rename_bytes: usize,
    pub sparse_flush_truncate_ops: usize,
    pub sparse_flush_truncate_bytes: usize,
    pub sparse_flush_cache_evict_ops: usize,
    pub sparse_flush_cache_evict_bytes: usize,
    pub sparse_flush_other_ops: usize,
    pub sparse_flush_other_bytes: usize,
    /// Successful rename barriers, split by the delayed state they actually
    /// published. A zero-byte barrier reached only table cleanup.
    pub rename_write_back_ops: usize,
    pub rename_sparse_flush_bytes: usize,
    pub rename_dense_write_back_bytes: usize,
    pub rename_zero_byte_fast_path_ops: usize,
    pub rename_path_cache_discard_ops: usize,
    /// Non-empty sparse flush batches, independent of their visibility
    /// barrier. `sparse_flush_ops` remains the number of individual runs.
    pub sparse_flush_batches: usize,
    pub sparse_flush_batch_runs: usize,
    pub sparse_flush_batch_bytes: usize,
    pub sparse_flush_batch_max_runs: usize,
    pub sparse_flush_batch_max_bytes: usize,
    /// Capacity-driven flushes split by the condition that could not accept
    /// the next sparse write. A batch that hits both bounds is counted in its
    /// own bucket so the two single-limit counters remain unambiguous.
    pub sparse_cache_evict_payload_limit_batches: usize,
    pub sparse_cache_evict_payload_limit_bytes: usize,
    pub sparse_cache_evict_run_limit_batches: usize,
    pub sparse_cache_evict_run_limit_bytes: usize,
    pub sparse_cache_evict_both_limits_batches: usize,
    pub sparse_cache_evict_both_limits_bytes: usize,
    pub sparse_cache_evict_allocation_failure_batches: usize,
    pub sparse_cache_evict_allocation_failure_bytes: usize,
    pub sparse_cache_evict_large_direct_batches: usize,
    pub sparse_cache_evict_large_direct_bytes: usize,
    pub sparse_cache_evict_global_budget_batches: usize,
    pub sparse_cache_evict_global_budget_bytes: usize,
    /// Failed sparse-buffer allocations include the initial-buffer case,
    /// which may not have a prior batch to flush.
    pub sparse_buffer_allocation_failure_ops: usize,
    pub sparse_buffer_allocation_failure_bytes: usize,
    pub sparse_large_direct_ops: usize,
    pub sparse_large_direct_bytes: usize,
    pub sparse_buffer_budget_direct_ops: usize,
    pub sparse_buffer_budget_direct_bytes: usize,
    pub sparse_buffer_resident_max_bytes: usize,
    /// Stages reached by `Ext4File::fstat()`.
    pub fstat_calls: usize,
    pub fstat_stat_get_ops: usize,
    pub fstat_write_back_overlay_ops: usize,
    pub fstat_write_back_fallback_ops: usize,
    pub sparse_read_overlay_ops: usize,
    pub sparse_read_overlay_bytes: usize,
    pub sparse_read_overlay_dirty_bytes: usize,
}

/// Direct-write fallback classification for the byte-only write-back cache.
#[cfg(feature = "perf")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum DirectWriteReason {
    Disabled,
    TooLarge,
    Uncached,
    Hole,
    Limit,
}

/// Successful `file_write_at()` path retained solely for fstat miss
/// attribution in Ya2yOS. It never participates in cache or I/O decisions.
#[cfg(feature = "perf")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum FileWritePath {
    DenseWriteBack,
    Direct,
    SparseBuffered,
}

/// Perf-only boundaries within [`crate::file::Ext4File::fstat`].
///
/// The wrapper has no dependency on Ya2yOS's architecture-specific tick
/// source.  It therefore emits only these boundaries; the kernel's perf
/// module supplies low-overhead aggregate timing around each paired event.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum FstatStageEvent {
    SparseWriteFlushBegin,
    SparseWriteFlushEnd,
    Ext4StatGetBegin,
    Ext4StatGetEnd,
    WriteBackFallbackBegin,
    WriteBackFallbackEnd,
    WriteBackOverlayBegin,
    WriteBackOverlayEnd,
}

/// Perf-only boundaries within the delayed-state barrier before rename.
///
/// The wrapper emits the events around the real sparse flush, dense byte-cache
/// write-back, and path-table discard. Ya2yOS supplies the tick accounting.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum RenameWriteBackStageEvent {
    SparseWriteFlushBegin,
    SparseWriteFlushEnd,
    DenseWriteBackBegin,
    DenseWriteBackEnd,
    PathCacheDiscardBegin,
    PathCacheDiscardEnd,
}

/// Internal bridge for wrapper fstat stage boundaries. The no-perf build uses
/// `()` so `file.rs` keeps one implementation of the observable fstat path.
#[cfg_attr(not(feature = "perf"), allow(dead_code))]
pub(crate) trait FstatStageObserver {
    fn stage(&mut self, event: FstatStageEvent);
}

/// Internal bridge for rename write-back stage boundaries. The no-perf build
/// uses `()` so `file.rs` keeps one rename implementation.
#[cfg_attr(not(feature = "perf"), allow(dead_code))]
pub(crate) trait RenameWriteBackStageObserver {
    fn stage(&mut self, event: RenameWriteBackStageEvent);
}

impl FstatStageObserver for () {
    #[inline]
    fn stage(&mut self, _event: FstatStageEvent) {}
}

#[cfg(feature = "perf")]
impl<F> FstatStageObserver for F
where
    F: FnMut(FstatStageEvent),
{
    #[inline]
    fn stage(&mut self, event: FstatStageEvent) {
        self(event);
    }
}

impl RenameWriteBackStageObserver for () {
    #[inline]
    fn stage(&mut self, _event: RenameWriteBackStageEvent) {}
}

#[cfg(feature = "perf")]
impl<F> RenameWriteBackStageObserver for F
where
    F: FnMut(RenameWriteBackStageEvent),
{
    #[inline]
    fn stage(&mut self, event: RenameWriteBackStageEvent) {
        self(event);
    }
}

/// Why a pending sparse-write range had to become visible to lwext4.
///
/// A full bounded range set is classified as cache eviction. Read, seek and
/// descriptor-switch barriers deliberately remain `Other`, keeping the
/// periodic report focused on fstat/close/rename/truncate causes.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum SparseWriteFlushReason {
    Fstat,
    Close,
    Rename,
    Truncate,
    CacheEvict,
    Other,
}

/// Exact reason a bounded sparse buffer was committed for capacity pressure.
/// This is perf-only attribution; it never selects a different write path.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum SparseWriteCacheEvictCause {
    PayloadLimit,
    RunLimit,
    BothLimits,
    AllocationFailure,
    LargeDirect,
    GlobalBudget,
}

#[cfg(feature = "perf")]
static WRITE_CACHE_HIT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_CACHE_HIT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_CACHE_FAST_HIT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_CACHE_FAST_HIT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_CACHE_INIT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_CACHE_INIT_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_CACHE_EVICT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_CACHE_EVICT_WRITEBACK_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_CACHE_LIMIT_FLUSH_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_CACHE_LIMIT_FLUSH_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_DISABLED_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_DISABLED_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_TOO_LARGE_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_TOO_LARGE_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_UNCACHED_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_UNCACHED_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_HOLE_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_HOLE_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_LIMIT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static WRITE_DIRECT_LIMIT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_BUFFER_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_BUFFER_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_FSTAT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_FSTAT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_CLOSE_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_CLOSE_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_RENAME_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_RENAME_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_TRUNCATE_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_TRUNCATE_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_CACHE_EVICT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_CACHE_EVICT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_OTHER_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_OTHER_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static RENAME_WRITE_BACK_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static RENAME_SPARSE_FLUSH_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static RENAME_DENSE_WRITE_BACK_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static RENAME_ZERO_BYTE_FAST_PATH_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static RENAME_PATH_CACHE_DISCARD_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_BATCHES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_BATCH_RUNS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_BATCH_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_BATCH_MAX_RUNS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_WRITE_FLUSH_BATCH_MAX_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BATCHES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_RUN_LIMIT_BATCHES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_RUN_LIMIT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_BOTH_LIMITS_BATCHES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_BOTH_LIMITS_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_ALLOCATION_FAILURE_BATCHES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_ALLOCATION_FAILURE_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_LARGE_DIRECT_BATCHES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_LARGE_DIRECT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BATCHES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_BUFFER_ALLOCATION_FAILURE_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_BUFFER_ALLOCATION_FAILURE_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_LARGE_DIRECT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_LARGE_DIRECT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_BUFFER_BUDGET_DIRECT_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_BUFFER_BUDGET_DIRECT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_BUFFER_RESIDENT_MAX_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static FSTAT_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static FSTAT_STAT_GET_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static FSTAT_WRITE_BACK_OVERLAY_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static FSTAT_WRITE_BACK_FALLBACK_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_READ_OVERLAY_OPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_READ_OVERLAY_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf")]
static SPARSE_READ_OVERLAY_DIRTY_BYTES: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_write_cache_hit(bytes: usize) {
    WRITE_CACHE_HIT_OPS.fetch_add(1, Ordering::Relaxed);
    WRITE_CACHE_HIT_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_write_cache_fast_hit(bytes: usize) {
    WRITE_CACHE_FAST_HIT_OPS.fetch_add(1, Ordering::Relaxed);
    WRITE_CACHE_FAST_HIT_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_write_cache_init(read_bytes: usize) {
    WRITE_CACHE_INIT_OPS.fetch_add(1, Ordering::Relaxed);
    WRITE_CACHE_INIT_READ_BYTES.fetch_add(read_bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_write_cache_eviction(writeback_bytes: usize) {
    WRITE_CACHE_EVICT_OPS.fetch_add(1, Ordering::Relaxed);
    WRITE_CACHE_EVICT_WRITEBACK_BYTES.fetch_add(writeback_bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_write_cache_limit_flush(bytes: usize) {
    WRITE_CACHE_LIMIT_FLUSH_OPS.fetch_add(1, Ordering::Relaxed);
    WRITE_CACHE_LIMIT_FLUSH_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_direct_write(reason: DirectWriteReason, bytes: usize) {
    WRITE_DIRECT_OPS.fetch_add(1, Ordering::Relaxed);
    WRITE_DIRECT_BYTES.fetch_add(bytes, Ordering::Relaxed);
    let (ops, total_bytes) = match reason {
        DirectWriteReason::Disabled => (&WRITE_DIRECT_DISABLED_OPS, &WRITE_DIRECT_DISABLED_BYTES),
        DirectWriteReason::TooLarge => (&WRITE_DIRECT_TOO_LARGE_OPS, &WRITE_DIRECT_TOO_LARGE_BYTES),
        DirectWriteReason::Uncached => (&WRITE_DIRECT_UNCACHED_OPS, &WRITE_DIRECT_UNCACHED_BYTES),
        DirectWriteReason::Hole => (&WRITE_DIRECT_HOLE_OPS, &WRITE_DIRECT_HOLE_BYTES),
        DirectWriteReason::Limit => (&WRITE_DIRECT_LIMIT_OPS, &WRITE_DIRECT_LIMIT_BYTES),
    };
    ops.fetch_add(1, Ordering::Relaxed);
    total_bytes.fetch_add(bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_sparse_write_buffer(bytes: usize) {
    SPARSE_WRITE_BUFFER_OPS.fetch_add(1, Ordering::Relaxed);
    SPARSE_WRITE_BUFFER_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
fn sparse_flush_reason_counters(
    reason: SparseWriteFlushReason,
) -> (&'static AtomicUsize, &'static AtomicUsize) {
    match reason {
        SparseWriteFlushReason::Fstat => (
            &SPARSE_WRITE_FLUSH_FSTAT_OPS,
            &SPARSE_WRITE_FLUSH_FSTAT_BYTES,
        ),
        SparseWriteFlushReason::Close => (
            &SPARSE_WRITE_FLUSH_CLOSE_OPS,
            &SPARSE_WRITE_FLUSH_CLOSE_BYTES,
        ),
        SparseWriteFlushReason::Rename => (
            &SPARSE_WRITE_FLUSH_RENAME_OPS,
            &SPARSE_WRITE_FLUSH_RENAME_BYTES,
        ),
        SparseWriteFlushReason::Truncate => (
            &SPARSE_WRITE_FLUSH_TRUNCATE_OPS,
            &SPARSE_WRITE_FLUSH_TRUNCATE_BYTES,
        ),
        SparseWriteFlushReason::CacheEvict => (
            &SPARSE_WRITE_FLUSH_CACHE_EVICT_OPS,
            &SPARSE_WRITE_FLUSH_CACHE_EVICT_BYTES,
        ),
        SparseWriteFlushReason::Other => (
            &SPARSE_WRITE_FLUSH_OTHER_OPS,
            &SPARSE_WRITE_FLUSH_OTHER_BYTES,
        ),
    }
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_sparse_write_flush_trigger(reason: SparseWriteFlushReason) {
    let (ops, _) = sparse_flush_reason_counters(reason);
    ops.fetch_add(1, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_sparse_write_flush(reason: SparseWriteFlushReason, bytes: usize) {
    SPARSE_WRITE_FLUSH_OPS.fetch_add(1, Ordering::Relaxed);
    SPARSE_WRITE_FLUSH_BYTES.fetch_add(bytes, Ordering::Relaxed);
    let (_, reason_bytes) = sparse_flush_reason_counters(reason);
    reason_bytes.fetch_add(bytes, Ordering::Relaxed);
}

/// Record one successful rename visibility barrier. The two byte counters are
/// kept separately because sparse extents and dense whole-file caches have
/// different correctness and optimization constraints.
#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_rename_write_back(sparse_bytes: usize, dense_bytes: usize) {
    RENAME_WRITE_BACK_OPS.fetch_add(1, Ordering::Relaxed);
    RENAME_SPARSE_FLUSH_BYTES.fetch_add(sparse_bytes, Ordering::Relaxed);
    RENAME_DENSE_WRITE_BACK_BYTES.fetch_add(dense_bytes, Ordering::Relaxed);
    if sparse_bytes == 0 && dense_bytes == 0 {
        RENAME_ZERO_BYTE_FAST_PATH_OPS.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_rename_path_cache_discard() {
    RENAME_PATH_CACHE_DISCARD_OPS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
fn record_max(counter: &AtomicUsize, value: usize) {
    let mut observed = counter.load(Ordering::Relaxed);
    while observed < value {
        match counter.compare_exchange_weak(observed, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => observed = actual,
        }
    }
}

#[cfg(feature = "perf")]
#[inline]
fn sparse_cache_evict_cause_counters(
    cause: SparseWriteCacheEvictCause,
) -> (&'static AtomicUsize, &'static AtomicUsize) {
    match cause {
        SparseWriteCacheEvictCause::PayloadLimit => (
            &SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BATCHES,
            &SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BYTES,
        ),
        SparseWriteCacheEvictCause::RunLimit => (
            &SPARSE_CACHE_EVICT_RUN_LIMIT_BATCHES,
            &SPARSE_CACHE_EVICT_RUN_LIMIT_BYTES,
        ),
        SparseWriteCacheEvictCause::BothLimits => (
            &SPARSE_CACHE_EVICT_BOTH_LIMITS_BATCHES,
            &SPARSE_CACHE_EVICT_BOTH_LIMITS_BYTES,
        ),
        SparseWriteCacheEvictCause::AllocationFailure => (
            &SPARSE_CACHE_EVICT_ALLOCATION_FAILURE_BATCHES,
            &SPARSE_CACHE_EVICT_ALLOCATION_FAILURE_BYTES,
        ),
        SparseWriteCacheEvictCause::LargeDirect => (
            &SPARSE_CACHE_EVICT_LARGE_DIRECT_BATCHES,
            &SPARSE_CACHE_EVICT_LARGE_DIRECT_BYTES,
        ),
        SparseWriteCacheEvictCause::GlobalBudget => (
            &SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BATCHES,
            &SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BYTES,
        ),
    }
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_sparse_write_flush_batch(
    cache_evict_cause: Option<SparseWriteCacheEvictCause>,
    runs: usize,
    bytes: usize,
) {
    SPARSE_WRITE_FLUSH_BATCHES.fetch_add(1, Ordering::Relaxed);
    SPARSE_WRITE_FLUSH_BATCH_RUNS.fetch_add(runs, Ordering::Relaxed);
    SPARSE_WRITE_FLUSH_BATCH_BYTES.fetch_add(bytes, Ordering::Relaxed);
    record_max(&SPARSE_WRITE_FLUSH_BATCH_MAX_RUNS, runs);
    record_max(&SPARSE_WRITE_FLUSH_BATCH_MAX_BYTES, bytes);
    if let Some(cause) = cache_evict_cause {
        let (batches, total_bytes) = sparse_cache_evict_cause_counters(cause);
        batches.fetch_add(1, Ordering::Relaxed);
        total_bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_sparse_buffer_allocation_failure(bytes: usize) {
    SPARSE_BUFFER_ALLOCATION_FAILURE_OPS.fetch_add(1, Ordering::Relaxed);
    SPARSE_BUFFER_ALLOCATION_FAILURE_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_sparse_large_direct(bytes: usize) {
    SPARSE_LARGE_DIRECT_OPS.fetch_add(1, Ordering::Relaxed);
    SPARSE_LARGE_DIRECT_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_sparse_buffer_budget_direct(bytes: usize) {
    SPARSE_BUFFER_BUDGET_DIRECT_OPS.fetch_add(1, Ordering::Relaxed);
    SPARSE_BUFFER_BUDGET_DIRECT_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_sparse_buffer_resident_bytes(bytes: usize) {
    record_max(&SPARSE_BUFFER_RESIDENT_MAX_BYTES, bytes);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_sparse_read_overlay(bytes: usize, dirty_bytes: usize) {
    SPARSE_READ_OVERLAY_OPS.fetch_add(1, Ordering::Relaxed);
    SPARSE_READ_OVERLAY_BYTES.fetch_add(bytes, Ordering::Relaxed);
    SPARSE_READ_OVERLAY_DIRTY_BYTES.fetch_add(dirty_bytes, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_fstat_call() {
    FSTAT_CALLS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_fstat_stat_get() {
    FSTAT_STAT_GET_OPS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_fstat_write_back_overlay() {
    FSTAT_WRITE_BACK_OVERLAY_OPS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(feature = "perf")]
#[inline]
pub(crate) fn record_fstat_write_back_fallback() {
    FSTAT_WRITE_BACK_FALLBACK_OPS.fetch_add(1, Ordering::Relaxed);
}

/// Return a relaxed aggregate snapshot for Ya2yOS's periodic perf report.
#[cfg(feature = "perf")]
pub fn write_back_cache_perf_stats() -> WriteBackCachePerfStats {
    WriteBackCachePerfStats {
        cache_hit_ops: WRITE_CACHE_HIT_OPS.load(Ordering::Relaxed),
        cache_hit_bytes: WRITE_CACHE_HIT_BYTES.load(Ordering::Relaxed),
        cache_fast_hit_ops: WRITE_CACHE_FAST_HIT_OPS.load(Ordering::Relaxed),
        cache_fast_hit_bytes: WRITE_CACHE_FAST_HIT_BYTES.load(Ordering::Relaxed),
        cache_init_ops: WRITE_CACHE_INIT_OPS.load(Ordering::Relaxed),
        cache_init_read_bytes: WRITE_CACHE_INIT_READ_BYTES.load(Ordering::Relaxed),
        cache_evict_ops: WRITE_CACHE_EVICT_OPS.load(Ordering::Relaxed),
        cache_evict_writeback_bytes: WRITE_CACHE_EVICT_WRITEBACK_BYTES.load(Ordering::Relaxed),
        cache_limit_flush_ops: WRITE_CACHE_LIMIT_FLUSH_OPS.load(Ordering::Relaxed),
        cache_limit_flush_bytes: WRITE_CACHE_LIMIT_FLUSH_BYTES.load(Ordering::Relaxed),
        direct_write_ops: WRITE_DIRECT_OPS.load(Ordering::Relaxed),
        direct_write_bytes: WRITE_DIRECT_BYTES.load(Ordering::Relaxed),
        direct_disabled_ops: WRITE_DIRECT_DISABLED_OPS.load(Ordering::Relaxed),
        direct_disabled_bytes: WRITE_DIRECT_DISABLED_BYTES.load(Ordering::Relaxed),
        direct_too_large_ops: WRITE_DIRECT_TOO_LARGE_OPS.load(Ordering::Relaxed),
        direct_too_large_bytes: WRITE_DIRECT_TOO_LARGE_BYTES.load(Ordering::Relaxed),
        direct_uncached_ops: WRITE_DIRECT_UNCACHED_OPS.load(Ordering::Relaxed),
        direct_uncached_bytes: WRITE_DIRECT_UNCACHED_BYTES.load(Ordering::Relaxed),
        direct_hole_ops: WRITE_DIRECT_HOLE_OPS.load(Ordering::Relaxed),
        direct_hole_bytes: WRITE_DIRECT_HOLE_BYTES.load(Ordering::Relaxed),
        direct_limit_ops: WRITE_DIRECT_LIMIT_OPS.load(Ordering::Relaxed),
        direct_limit_bytes: WRITE_DIRECT_LIMIT_BYTES.load(Ordering::Relaxed),
        sparse_buffer_ops: SPARSE_WRITE_BUFFER_OPS.load(Ordering::Relaxed),
        sparse_buffer_bytes: SPARSE_WRITE_BUFFER_BYTES.load(Ordering::Relaxed),
        sparse_flush_ops: SPARSE_WRITE_FLUSH_OPS.load(Ordering::Relaxed),
        sparse_flush_bytes: SPARSE_WRITE_FLUSH_BYTES.load(Ordering::Relaxed),
        sparse_flush_fstat_ops: SPARSE_WRITE_FLUSH_FSTAT_OPS.load(Ordering::Relaxed),
        sparse_flush_fstat_bytes: SPARSE_WRITE_FLUSH_FSTAT_BYTES.load(Ordering::Relaxed),
        sparse_flush_close_ops: SPARSE_WRITE_FLUSH_CLOSE_OPS.load(Ordering::Relaxed),
        sparse_flush_close_bytes: SPARSE_WRITE_FLUSH_CLOSE_BYTES.load(Ordering::Relaxed),
        sparse_flush_rename_ops: SPARSE_WRITE_FLUSH_RENAME_OPS.load(Ordering::Relaxed),
        sparse_flush_rename_bytes: SPARSE_WRITE_FLUSH_RENAME_BYTES.load(Ordering::Relaxed),
        sparse_flush_truncate_ops: SPARSE_WRITE_FLUSH_TRUNCATE_OPS.load(Ordering::Relaxed),
        sparse_flush_truncate_bytes: SPARSE_WRITE_FLUSH_TRUNCATE_BYTES.load(Ordering::Relaxed),
        sparse_flush_cache_evict_ops: SPARSE_WRITE_FLUSH_CACHE_EVICT_OPS.load(Ordering::Relaxed),
        sparse_flush_cache_evict_bytes: SPARSE_WRITE_FLUSH_CACHE_EVICT_BYTES
            .load(Ordering::Relaxed),
        sparse_flush_other_ops: SPARSE_WRITE_FLUSH_OTHER_OPS.load(Ordering::Relaxed),
        sparse_flush_other_bytes: SPARSE_WRITE_FLUSH_OTHER_BYTES.load(Ordering::Relaxed),
        rename_write_back_ops: RENAME_WRITE_BACK_OPS.load(Ordering::Relaxed),
        rename_sparse_flush_bytes: RENAME_SPARSE_FLUSH_BYTES.load(Ordering::Relaxed),
        rename_dense_write_back_bytes: RENAME_DENSE_WRITE_BACK_BYTES.load(Ordering::Relaxed),
        rename_zero_byte_fast_path_ops: RENAME_ZERO_BYTE_FAST_PATH_OPS.load(Ordering::Relaxed),
        rename_path_cache_discard_ops: RENAME_PATH_CACHE_DISCARD_OPS.load(Ordering::Relaxed),
        sparse_flush_batches: SPARSE_WRITE_FLUSH_BATCHES.load(Ordering::Relaxed),
        sparse_flush_batch_runs: SPARSE_WRITE_FLUSH_BATCH_RUNS.load(Ordering::Relaxed),
        sparse_flush_batch_bytes: SPARSE_WRITE_FLUSH_BATCH_BYTES.load(Ordering::Relaxed),
        sparse_flush_batch_max_runs: SPARSE_WRITE_FLUSH_BATCH_MAX_RUNS.load(Ordering::Relaxed),
        sparse_flush_batch_max_bytes: SPARSE_WRITE_FLUSH_BATCH_MAX_BYTES.load(Ordering::Relaxed),
        sparse_cache_evict_payload_limit_batches: SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BATCHES
            .load(Ordering::Relaxed),
        sparse_cache_evict_payload_limit_bytes: SPARSE_CACHE_EVICT_PAYLOAD_LIMIT_BYTES
            .load(Ordering::Relaxed),
        sparse_cache_evict_run_limit_batches: SPARSE_CACHE_EVICT_RUN_LIMIT_BATCHES
            .load(Ordering::Relaxed),
        sparse_cache_evict_run_limit_bytes: SPARSE_CACHE_EVICT_RUN_LIMIT_BYTES
            .load(Ordering::Relaxed),
        sparse_cache_evict_both_limits_batches: SPARSE_CACHE_EVICT_BOTH_LIMITS_BATCHES
            .load(Ordering::Relaxed),
        sparse_cache_evict_both_limits_bytes: SPARSE_CACHE_EVICT_BOTH_LIMITS_BYTES
            .load(Ordering::Relaxed),
        sparse_cache_evict_allocation_failure_batches:
            SPARSE_CACHE_EVICT_ALLOCATION_FAILURE_BATCHES.load(Ordering::Relaxed),
        sparse_cache_evict_allocation_failure_bytes: SPARSE_CACHE_EVICT_ALLOCATION_FAILURE_BYTES
            .load(Ordering::Relaxed),
        sparse_cache_evict_large_direct_batches: SPARSE_CACHE_EVICT_LARGE_DIRECT_BATCHES
            .load(Ordering::Relaxed),
        sparse_cache_evict_large_direct_bytes: SPARSE_CACHE_EVICT_LARGE_DIRECT_BYTES
            .load(Ordering::Relaxed),
        sparse_cache_evict_global_budget_batches: SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BATCHES
            .load(Ordering::Relaxed),
        sparse_cache_evict_global_budget_bytes: SPARSE_CACHE_EVICT_GLOBAL_BUDGET_BYTES
            .load(Ordering::Relaxed),
        sparse_buffer_allocation_failure_ops: SPARSE_BUFFER_ALLOCATION_FAILURE_OPS
            .load(Ordering::Relaxed),
        sparse_buffer_allocation_failure_bytes: SPARSE_BUFFER_ALLOCATION_FAILURE_BYTES
            .load(Ordering::Relaxed),
        sparse_large_direct_ops: SPARSE_LARGE_DIRECT_OPS.load(Ordering::Relaxed),
        sparse_large_direct_bytes: SPARSE_LARGE_DIRECT_BYTES.load(Ordering::Relaxed),
        sparse_buffer_budget_direct_ops: SPARSE_BUFFER_BUDGET_DIRECT_OPS.load(Ordering::Relaxed),
        sparse_buffer_budget_direct_bytes: SPARSE_BUFFER_BUDGET_DIRECT_BYTES
            .load(Ordering::Relaxed),
        sparse_buffer_resident_max_bytes: SPARSE_BUFFER_RESIDENT_MAX_BYTES.load(Ordering::Relaxed),
        fstat_calls: FSTAT_CALLS.load(Ordering::Relaxed),
        fstat_stat_get_ops: FSTAT_STAT_GET_OPS.load(Ordering::Relaxed),
        fstat_write_back_overlay_ops: FSTAT_WRITE_BACK_OVERLAY_OPS.load(Ordering::Relaxed),
        fstat_write_back_fallback_ops: FSTAT_WRITE_BACK_FALLBACK_OPS.load(Ordering::Relaxed),
        sparse_read_overlay_ops: SPARSE_READ_OVERLAY_OPS.load(Ordering::Relaxed),
        sparse_read_overlay_bytes: SPARSE_READ_OVERLAY_BYTES.load(Ordering::Relaxed),
        sparse_read_overlay_dirty_bytes: SPARSE_READ_OVERLAY_DIRTY_BYTES.load(Ordering::Relaxed),
    }
}
