//! Filesystem performance counters: lwext4, VFS/page cache, and pipe I/O.

use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "perf")]
use core::sync::atomic::AtomicBool;

#[cfg(feature = "perf")]
use lwext4_rust::perf::{FstatStageEvent, RenameWriteBackStageEvent};

use crate::arch::time::get_ticks;

use super::common::{add, emit_duration, record_duration, update_max};
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

/// Successful VFS-level `Inode::read_at()` calls, partitioned by their caller.
///
/// The buckets deliberately identify a small number of stable read paths
/// rather than files, processes, or pages. They therefore stay suitable for
/// a contended BuildStorm run while explaining which callers feed lwext4's
/// read-data gate.
pub(crate) struct InodeReadSourceStats {
    pub(crate) ops: AtomicUsize,
    pub(crate) bytes: AtomicUsize,
}

impl InodeReadSourceStats {
    const fn new() -> Self {
        Self {
            ops: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn record(&self, bytes: usize) {
        add(&self.ops, 1);
        add(&self.bytes, bytes);
    }
}

/// Coarse caller classes for a real `Inode::read_at()` invocation.
#[derive(Clone, Copy)]
pub enum InodeReadSource {
    MmapDemand,
    MmapPrefetch,
    PageCachedReadColdRun,
    DirectBypass,
    Other,
}

pub(crate) static INODE_READ_MMAP_CACHE_FILL: InodeReadSourceStats = InodeReadSourceStats::new();
pub(crate) static INODE_READ_MMAP_DEMAND: InodeReadSourceStats = InodeReadSourceStats::new();
pub(crate) static INODE_READ_MMAP_PREFETCH: InodeReadSourceStats = InodeReadSourceStats::new();
pub(crate) static INODE_READ_PAGE_CACHED_COLD_RUN: InodeReadSourceStats =
    InodeReadSourceStats::new();
pub(crate) static INODE_READ_DIRECT_BYPASS: InodeReadSourceStats = InodeReadSourceStats::new();
pub(crate) static INODE_READ_OTHER: InodeReadSourceStats = InodeReadSourceStats::new();

/// Aggregate duration for one named lwext4 or VFS phase.
///
/// Phase counters intentionally remain process- and caller-free.  Their role
/// is to identify contended pathname and metadata paths without per-call logs.
pub(crate) struct Ext4PhaseStats {
    pub(crate) samples: AtomicUsize,
    pub(crate) ticks: AtomicUsize,
    pub(crate) max_ticks: AtomicUsize,
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

/// Request classes crossing the serialized physical block-device boundary.
#[cfg(feature = "perf")]
#[derive(Clone, Copy)]
pub(crate) enum Ext4BlockRequestKind {
    Read,
    Write,
    Flush,
}

/// Aggregate queue and service costs for `Disk::dev` requests.
#[cfg(feature = "perf")]
pub(crate) struct Ext4BlockDeviceStats {
    pub(crate) submits: AtomicUsize,
    pub(crate) read_requests: AtomicUsize,
    pub(crate) write_requests: AtomicUsize,
    pub(crate) flush_requests: AtomicUsize,
    pub(crate) completed: AtomicUsize,
    pub(crate) contended: AtomicUsize,
    pub(crate) queued: AtomicUsize,
    pub(crate) max_queue_depth: AtomicUsize,
    pub(crate) bytes: AtomicUsize,
    pub(crate) errors: AtomicUsize,
    pub(crate) wait_ticks: AtomicUsize,
    pub(crate) max_wait_ticks: AtomicUsize,
    pub(crate) service_ticks: AtomicUsize,
    pub(crate) max_service_ticks: AtomicUsize,
}

#[cfg(feature = "perf")]
impl Ext4BlockDeviceStats {
    const fn new() -> Self {
        Self {
            submits: AtomicUsize::new(0),
            read_requests: AtomicUsize::new(0),
            write_requests: AtomicUsize::new(0),
            flush_requests: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            contended: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            max_queue_depth: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            errors: AtomicUsize::new(0),
            wait_ticks: AtomicUsize::new(0),
            max_wait_ticks: AtomicUsize::new(0),
            service_ticks: AtomicUsize::new(0),
            max_service_ticks: AtomicUsize::new(0),
        }
    }

    fn reset(&self) {
        self.submits.store(0, Ordering::Relaxed);
        self.read_requests.store(0, Ordering::Relaxed);
        self.write_requests.store(0, Ordering::Relaxed);
        self.flush_requests.store(0, Ordering::Relaxed);
        self.completed.store(0, Ordering::Relaxed);
        self.contended.store(0, Ordering::Relaxed);
        self.queued.store(0, Ordering::Relaxed);
        self.max_queue_depth.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
        self.errors.store(0, Ordering::Relaxed);
        self.wait_ticks.store(0, Ordering::Relaxed);
        self.max_wait_ticks.store(0, Ordering::Relaxed);
        self.service_ticks.store(0, Ordering::Relaxed);
        self.max_service_ticks.store(0, Ordering::Relaxed);
    }
}

/// Cause retained with a regular-file stat-cache miss until `fstat()` really
/// enters `Ext4File::fstat()`. Values are operation classes, never paths or
/// tasks, so the counters remain low-overhead global aggregates.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Ext4FstatMissReason {
    ColdInode,
    DenseWriteBack,
    DirectWrite,
    SparseBufferedWrite,
    Truncate,
    Rename,
    Unlink,
    HardLink,
    Metadata,
    AliasRecovery,
    FsIndexRebuild,
}

/// Inode class for the subset of actual `ColdInode` fstat misses.  This stays
/// independent of VFS types so the perf module does not add a filesystem
/// dependency; Ext4 maps its own `InodeType` at the recording site.
#[derive(Clone, Copy)]
pub enum Ext4FstatColdInodeKind {
    RegularFile,
    Directory,
    SpecialNode,
}

/// Actual ColdInode fstat misses split by inode class and whether construction
/// carried the stat obtained by pathname lookup.  The six counters are a
/// diagnostic partition: their sum must equal the `cold_inode` samples in
/// `EXT4_FSTAT_INNER_MISSES` for each perf snapshot.
pub(crate) struct Ext4FstatColdInodeCounts {
    pub(crate) regular_lookup_stat: AtomicUsize,
    pub(crate) regular_no_lookup_stat: AtomicUsize,
    pub(crate) directory_lookup_stat: AtomicUsize,
    pub(crate) directory_no_lookup_stat: AtomicUsize,
    pub(crate) special_lookup_stat: AtomicUsize,
    pub(crate) special_no_lookup_stat: AtomicUsize,
}

impl Ext4FstatColdInodeCounts {
    const fn new() -> Self {
        Self {
            regular_lookup_stat: AtomicUsize::new(0),
            regular_no_lookup_stat: AtomicUsize::new(0),
            directory_lookup_stat: AtomicUsize::new(0),
            directory_no_lookup_stat: AtomicUsize::new(0),
            special_lookup_stat: AtomicUsize::new(0),
            special_no_lookup_stat: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn record(&self, kind: Ext4FstatColdInodeKind, has_lookup_stat: bool) {
        let counter = match (kind, has_lookup_stat) {
            (Ext4FstatColdInodeKind::RegularFile, true) => &self.regular_lookup_stat,
            (Ext4FstatColdInodeKind::RegularFile, false) => &self.regular_no_lookup_stat,
            (Ext4FstatColdInodeKind::Directory, true) => &self.directory_lookup_stat,
            (Ext4FstatColdInodeKind::Directory, false) => &self.directory_no_lookup_stat,
            (Ext4FstatColdInodeKind::SpecialNode, true) => &self.special_lookup_stat,
            (Ext4FstatColdInodeKind::SpecialNode, false) => &self.special_no_lookup_stat,
        };
        add(counter, 1);
    }
}

/// Cache-invalidation counts grouped by their source. Every successful
/// metadata-changing operation is included, even if a prior invalidation has
/// not yet been consumed by `fstat()`.
pub(crate) struct Ext4FstatReasonCounts {
    pub(crate) cold_inode: AtomicUsize,
    pub(crate) dense_write_back: AtomicUsize,
    pub(crate) direct_write: AtomicUsize,
    pub(crate) sparse_buffered_write: AtomicUsize,
    pub(crate) truncate: AtomicUsize,
    pub(crate) rename: AtomicUsize,
    pub(crate) unlink: AtomicUsize,
    pub(crate) hard_link: AtomicUsize,
    pub(crate) metadata: AtomicUsize,
    pub(crate) alias_recovery: AtomicUsize,
    pub(crate) fsidx_rebuild: AtomicUsize,
}

impl Ext4FstatReasonCounts {
    const fn new() -> Self {
        Self {
            cold_inode: AtomicUsize::new(0),
            dense_write_back: AtomicUsize::new(0),
            direct_write: AtomicUsize::new(0),
            sparse_buffered_write: AtomicUsize::new(0),
            truncate: AtomicUsize::new(0),
            rename: AtomicUsize::new(0),
            unlink: AtomicUsize::new(0),
            hard_link: AtomicUsize::new(0),
            metadata: AtomicUsize::new(0),
            alias_recovery: AtomicUsize::new(0),
            fsidx_rebuild: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn record(&self, reason: Ext4FstatMissReason) {
        let counter = match reason {
            Ext4FstatMissReason::ColdInode => &self.cold_inode,
            Ext4FstatMissReason::DenseWriteBack => &self.dense_write_back,
            Ext4FstatMissReason::DirectWrite => &self.direct_write,
            Ext4FstatMissReason::SparseBufferedWrite => &self.sparse_buffered_write,
            Ext4FstatMissReason::Truncate => &self.truncate,
            Ext4FstatMissReason::Rename => &self.rename,
            Ext4FstatMissReason::Unlink => &self.unlink,
            Ext4FstatMissReason::HardLink => &self.hard_link,
            Ext4FstatMissReason::Metadata => &self.metadata,
            Ext4FstatMissReason::AliasRecovery => &self.alias_recovery,
            Ext4FstatMissReason::FsIndexRebuild => &self.fsidx_rebuild,
        };
        add(counter, 1);
    }
}

/// Actual `Ext4File::fstat()` time grouped by the reason that reached it. A
/// stale pathname can yield two samples: the failed old alias and its explicit
/// `AliasRecovery` retry.
pub(crate) struct Ext4FstatReasonPhases {
    pub(crate) cold_inode: Ext4PhaseStats,
    pub(crate) dense_write_back: Ext4PhaseStats,
    pub(crate) direct_write: Ext4PhaseStats,
    pub(crate) sparse_buffered_write: Ext4PhaseStats,
    pub(crate) truncate: Ext4PhaseStats,
    pub(crate) rename: Ext4PhaseStats,
    pub(crate) unlink: Ext4PhaseStats,
    pub(crate) hard_link: Ext4PhaseStats,
    pub(crate) metadata: Ext4PhaseStats,
    pub(crate) alias_recovery: Ext4PhaseStats,
    pub(crate) fsidx_rebuild: Ext4PhaseStats,
}

impl Ext4FstatReasonPhases {
    const fn new() -> Self {
        Self {
            cold_inode: Ext4PhaseStats::new(),
            dense_write_back: Ext4PhaseStats::new(),
            direct_write: Ext4PhaseStats::new(),
            sparse_buffered_write: Ext4PhaseStats::new(),
            truncate: Ext4PhaseStats::new(),
            rename: Ext4PhaseStats::new(),
            unlink: Ext4PhaseStats::new(),
            hard_link: Ext4PhaseStats::new(),
            metadata: Ext4PhaseStats::new(),
            alias_recovery: Ext4PhaseStats::new(),
            fsidx_rebuild: Ext4PhaseStats::new(),
        }
    }

    #[inline]
    fn record(&self, reason: Ext4FstatMissReason, elapsed: usize) {
        let stats = match reason {
            Ext4FstatMissReason::ColdInode => &self.cold_inode,
            Ext4FstatMissReason::DenseWriteBack => &self.dense_write_back,
            Ext4FstatMissReason::DirectWrite => &self.direct_write,
            Ext4FstatMissReason::SparseBufferedWrite => &self.sparse_buffered_write,
            Ext4FstatMissReason::Truncate => &self.truncate,
            Ext4FstatMissReason::Rename => &self.rename,
            Ext4FstatMissReason::Unlink => &self.unlink,
            Ext4FstatMissReason::HardLink => &self.hard_link,
            Ext4FstatMissReason::Metadata => &self.metadata,
            Ext4FstatMissReason::AliasRecovery => &self.alias_recovery,
            Ext4FstatMissReason::FsIndexRebuild => &self.fsidx_rebuild,
        };
        stats.record(elapsed);
    }
}

#[cfg(feature = "perf")]
pub(crate) static EXT4_BLOCK_DEVICE_STATS: Ext4BlockDeviceStats = Ext4BlockDeviceStats::new();
#[cfg(feature = "perf")]
static EXT4_BLOCK_DEVICE_PERF_ENABLED: AtomicBool = AtomicBool::new(false);

/// Aggregate costs of task-aware lwext4 resource locks and their Rust address
/// registry. The data is intentionally not keyed by path, inode, or task so it
/// remains suitable for long SMP BuildStorm runs.
#[cfg(feature = "perf")]
pub(crate) struct Ext4ResourceLockStats {
    pub(crate) acquires: AtomicUsize,
    pub(crate) contended: AtomicUsize,
    pub(crate) queued: AtomicUsize,
    pub(crate) max_queue_depth: AtomicUsize,
    pub(crate) wait_ticks: AtomicUsize,
    pub(crate) max_wait_ticks: AtomicUsize,
    pub(crate) hold_ticks: AtomicUsize,
    pub(crate) max_hold_ticks: AtomicUsize,
    pub(crate) registry_lookups: AtomicUsize,
    pub(crate) registry_creates: AtomicUsize,
    pub(crate) registry_ticks: AtomicUsize,
    pub(crate) max_registry_ticks: AtomicUsize,
}

#[cfg(feature = "perf")]
impl Ext4ResourceLockStats {
    const fn new() -> Self {
        Self {
            acquires: AtomicUsize::new(0),
            contended: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            max_queue_depth: AtomicUsize::new(0),
            wait_ticks: AtomicUsize::new(0),
            max_wait_ticks: AtomicUsize::new(0),
            hold_ticks: AtomicUsize::new(0),
            max_hold_ticks: AtomicUsize::new(0),
            registry_lookups: AtomicUsize::new(0),
            registry_creates: AtomicUsize::new(0),
            registry_ticks: AtomicUsize::new(0),
            max_registry_ticks: AtomicUsize::new(0),
        }
    }

    fn reset(&self) {
        self.acquires.store(0, Ordering::Relaxed);
        self.contended.store(0, Ordering::Relaxed);
        self.queued.store(0, Ordering::Relaxed);
        self.max_queue_depth.store(0, Ordering::Relaxed);
        self.wait_ticks.store(0, Ordering::Relaxed);
        self.max_wait_ticks.store(0, Ordering::Relaxed);
        self.hold_ticks.store(0, Ordering::Relaxed);
        self.max_hold_ticks.store(0, Ordering::Relaxed);
        self.registry_lookups.store(0, Ordering::Relaxed);
        self.registry_creates.store(0, Ordering::Relaxed);
        self.registry_ticks.store(0, Ordering::Relaxed);
        self.max_registry_ticks.store(0, Ordering::Relaxed);
    }
}

#[cfg(feature = "perf")]
pub(crate) static EXT4_RESOURCE_LOCK_STATS: Ext4ResourceLockStats = Ext4ResourceLockStats::new();
#[cfg(feature = "perf")]
static EXT4_RESOURCE_LOCK_PERF_ENABLED: AtomicBool = AtomicBool::new(false);
/// `fast_cached`, `directory_epoch_cached`, `post_wait_cached`, and
/// `actual_ext4_fstat` are mutually exclusive results of `Ext4Inode::fstat()`.
/// Alias recovery is a nested subphase of the last bucket and is deliberately
/// reported separately.
pub(crate) static EXT4_FSTAT_FAST_CACHED: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_DIRECTORY_EPOCH_CACHED: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_POST_WAIT_CACHED: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_POST_WAIT_DIRECTORY_CACHED: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_ACTUAL_EXT4_FSTAT: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_RECOVER_LIVE_PATH: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_SPARSE_WRITE_FLUSH: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_STAT_GET: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_WRITE_BACK_FALLBACK: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_WRITE_BACK_OVERLAY: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_FSTAT_CACHE_INVALIDATIONS: Ext4FstatReasonCounts =
    Ext4FstatReasonCounts::new();
pub(crate) static EXT4_FSTAT_INNER_MISSES: Ext4FstatReasonPhases = Ext4FstatReasonPhases::new();
pub(crate) static EXT4_FSTAT_COLD_INODE_COUNTS: Ext4FstatColdInodeCounts =
    Ext4FstatColdInodeCounts::new();
/// Directory stat cache samples discarded because a directory metadata
/// operation completed after they were captured.
pub(crate) static EXT4_FSTAT_DIRECTORY_STAT_EPOCH_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_FSTAT_DIRECTORY_STAT_LOCAL_EPOCH_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_FSTAT_DIRECTORY_STAT_GLOBAL_EPOCH_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_FSTAT_DIRECTORY_PARENT_LOCAL_UPDATES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXT4_FSTAT_DIRECTORY_PARENT_GLOBAL_FALLBACKS: AtomicUsize = AtomicUsize::new(0);
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
pub(crate) static EXT4_RENAME_SPARSE_WRITE_FLUSH: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_RENAME_DENSE_WRITE_BACK: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_RENAME_PATH_CACHE_DISCARD: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_RENAME_CLOSE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_RENAME_LWEXT4_RENAME: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_RENAME_VFS_CACHE_INVALIDATE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_CREATE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_CREATE_EXIST_CHECK: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_CREATE_NODE_OPEN: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_CREATE_FILE_CLOSE: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_CREATE_METADATA_APPLY: Ext4PhaseStats = Ext4PhaseStats::new();
pub(crate) static EXT4_NAMESPACE_CREATE_VFS_FINISH: Ext4PhaseStats = Ext4PhaseStats::new();
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
pub(crate) static FILE_CACHE_CAPACITY_BYPASS_PAGES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_EVICTIONS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_EVICTION_SCANS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_EVICTION_SECOND_CHANCES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_EVICTION_DIRTY_SKIPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_EVICTION_IN_USE_SKIPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_EVICTION_DEFERRED_RETRY_PAGES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_EVICTION_COOLDOWN_BYPASSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_PAGE_FAULTS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READAHEAD_OPS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READAHEAD_PAGES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FILE_CACHE_READAHEAD_BYTES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_POSITIVE_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_NEGATIVE_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_POSITIVE_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_NEGATIVE_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_INVALIDATES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_INVALIDATE_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_CLEAR_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_PARENT_MISSES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_DENTRY_LOOKUP_BYPASS_FLAGS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_PATH_INDEX_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_CACHED_PARENT_FINDS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_ROOT_FINDS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_PRESERVE_FINAL_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_RECLAIMED: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_REBUILDS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_IDENTITY_EPOCH_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_IDENTITY_LIVE_PROBES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFS_FSINDEX_IDENTITY_STALE_REPLACES: AtomicUsize = AtomicUsize::new(0);
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

pub fn record_ext4_read(bytes: usize) {
    add(&EXT4_READ_OPS, 1);
    add(&EXT4_READ_BYTES, bytes);
}

/// Record one real VFS-level `Inode::read_at()` return. Page-cache hits do not
/// call this API, so this is intentionally not a per-page lookup counter.
#[inline]
pub fn record_inode_read_source(source: InodeReadSource, bytes: usize) {
    match source {
        InodeReadSource::MmapDemand => {
            INODE_READ_MMAP_CACHE_FILL.record(bytes);
            INODE_READ_MMAP_DEMAND.record(bytes);
        }
        InodeReadSource::MmapPrefetch => {
            INODE_READ_MMAP_CACHE_FILL.record(bytes);
            INODE_READ_MMAP_PREFETCH.record(bytes);
        }
        InodeReadSource::PageCachedReadColdRun => INODE_READ_PAGE_CACHED_COLD_RUN.record(bytes),
        InodeReadSource::DirectBypass => INODE_READ_DIRECT_BYPASS.record(bytes),
        InodeReadSource::Other => INODE_READ_OTHER.record(bytes),
    }
}

#[inline]
pub fn record_ext4_byte_cache_read_hit(bytes: usize) {
    add(&EXT4_BYTE_CACHE_READ_HITS, 1);
    add(&EXT4_BYTE_CACHE_READ_HIT_BYTES, bytes);
}

/// A mutually exclusive phase inside `Ext4Inode::write_at()`.
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

/// Result classes for one `Ext4Inode::fstat()` call. The recorded duration
/// starts at method entry, so the non-fast buckets include time spent waiting
/// for inode state and the mount-wide lwext4 gate.
pub enum Ext4FstatPath {
    FastCached,
    DirectoryEpochCached,
    PostWaitCached,
    PostWaitDirectoryCached,
    ActualExt4Fstat,
}

/// Records exactly one terminal result for one `Ext4Inode::fstat()` call.
pub struct Ext4FstatPathGuard {
    begin: usize,
}

impl Ext4FstatPathGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }

    #[inline]
    pub fn finish(self, path: Ext4FstatPath) {
        let elapsed = get_ticks().saturating_sub(self.begin);
        match path {
            Ext4FstatPath::FastCached => EXT4_FSTAT_FAST_CACHED.record(elapsed),
            Ext4FstatPath::DirectoryEpochCached => {
                EXT4_FSTAT_DIRECTORY_EPOCH_CACHED.record(elapsed)
            }
            Ext4FstatPath::PostWaitCached => EXT4_FSTAT_POST_WAIT_CACHED.record(elapsed),
            Ext4FstatPath::PostWaitDirectoryCached => {
                EXT4_FSTAT_POST_WAIT_DIRECTORY_CACHED.record(elapsed)
            }
            Ext4FstatPath::ActualExt4Fstat => EXT4_FSTAT_ACTUAL_EXT4_FSTAT.record(elapsed),
        }
    }
}

/// Times the alias scan and descriptor replacement after `Ext4File::fstat()`
/// reports a stale pathname. This is intentionally separate from the
/// mutually exclusive terminal result above.
pub struct Ext4FstatRecoveryGuard {
    begin: usize,
}

impl Ext4FstatRecoveryGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for Ext4FstatRecoveryGuard {
    #[inline]
    fn drop(&mut self) {
        EXT4_FSTAT_RECOVER_LIVE_PATH.record(get_ticks().saturating_sub(self.begin));
    }
}

/// Time exactly one real entry into `Ext4File::fstat()`. Unlike
/// [`Ext4FstatPathGuard`], this excludes inode/global-lock waiting and lets a
/// report separate `ext4_stat_get` candidates from a sparse flush performed
/// inside the wrapper.
pub struct Ext4FstatMissGuard {
    reason: Ext4FstatMissReason,
    begin: usize,
}

impl Ext4FstatMissGuard {
    #[inline]
    pub fn new(reason: Ext4FstatMissReason) -> Self {
        Self {
            reason,
            begin: get_ticks(),
        }
    }
}

impl Drop for Ext4FstatMissGuard {
    #[inline]
    fn drop(&mut self) {
        EXT4_FSTAT_INNER_MISSES.record(self.reason, get_ticks().saturating_sub(self.begin));
    }
}

/// Per-call timing state for `Ext4File::fstat()` boundaries. It lives on the
/// caller stack rather than in a global state, so timing remains correct if a
/// later implementation ever removes the ext4 mount-wide serialization.
#[cfg(feature = "perf")]
pub struct Ext4FstatStageRecorder {
    sparse_write_flush_begin: usize,
    stat_get_begin: usize,
    write_back_fallback_begin: usize,
    write_back_overlay_begin: usize,
}

#[cfg(feature = "perf")]
impl Ext4FstatStageRecorder {
    #[inline]
    pub fn new() -> Self {
        Self {
            sparse_write_flush_begin: 0,
            stat_get_begin: 0,
            write_back_fallback_begin: 0,
            write_back_overlay_begin: 0,
        }
    }

    #[inline]
    pub fn record(&mut self, event: FstatStageEvent) {
        match event {
            FstatStageEvent::SparseWriteFlushBegin => {
                self.sparse_write_flush_begin = get_ticks();
            }
            FstatStageEvent::SparseWriteFlushEnd => {
                EXT4_FSTAT_SPARSE_WRITE_FLUSH
                    .record(get_ticks().saturating_sub(self.sparse_write_flush_begin));
            }
            FstatStageEvent::Ext4StatGetBegin => {
                self.stat_get_begin = get_ticks();
            }
            FstatStageEvent::Ext4StatGetEnd => {
                EXT4_FSTAT_STAT_GET.record(get_ticks().saturating_sub(self.stat_get_begin));
            }
            FstatStageEvent::WriteBackFallbackBegin => {
                self.write_back_fallback_begin = get_ticks();
            }
            FstatStageEvent::WriteBackFallbackEnd => {
                EXT4_FSTAT_WRITE_BACK_FALLBACK
                    .record(get_ticks().saturating_sub(self.write_back_fallback_begin));
            }
            FstatStageEvent::WriteBackOverlayBegin => {
                self.write_back_overlay_begin = get_ticks();
            }
            FstatStageEvent::WriteBackOverlayEnd => {
                EXT4_FSTAT_WRITE_BACK_OVERLAY
                    .record(get_ticks().saturating_sub(self.write_back_overlay_begin));
            }
        }
    }
}

/// Per-call timing state for the delayed-state visibility barrier in
/// `Ext4Inode::rename()`. This follows the wrapper's synchronous events so
/// each bucket excludes unrelated rename work and lock wait time.
#[cfg(feature = "perf")]
pub struct Ext4RenameWriteBackStageRecorder {
    sparse_write_flush_begin: usize,
    dense_write_back_begin: usize,
    path_cache_discard_begin: usize,
}

#[cfg(feature = "perf")]
impl Ext4RenameWriteBackStageRecorder {
    #[inline]
    pub fn new() -> Self {
        Self {
            sparse_write_flush_begin: 0,
            dense_write_back_begin: 0,
            path_cache_discard_begin: 0,
        }
    }

    #[inline]
    pub fn record(&mut self, event: RenameWriteBackStageEvent) {
        match event {
            RenameWriteBackStageEvent::SparseWriteFlushBegin => {
                self.sparse_write_flush_begin = get_ticks();
            }
            RenameWriteBackStageEvent::SparseWriteFlushEnd => {
                EXT4_RENAME_SPARSE_WRITE_FLUSH
                    .record(get_ticks().saturating_sub(self.sparse_write_flush_begin));
            }
            RenameWriteBackStageEvent::DenseWriteBackBegin => {
                self.dense_write_back_begin = get_ticks();
            }
            RenameWriteBackStageEvent::DenseWriteBackEnd => {
                EXT4_RENAME_DENSE_WRITE_BACK
                    .record(get_ticks().saturating_sub(self.dense_write_back_begin));
            }
            RenameWriteBackStageEvent::PathCacheDiscardBegin => {
                self.path_cache_discard_begin = get_ticks();
            }
            RenameWriteBackStageEvent::PathCacheDiscardEnd => {
                EXT4_RENAME_PATH_CACHE_DISCARD
                    .record(get_ticks().saturating_sub(self.path_cache_discard_begin));
            }
        }
    }
}

/// Record the operation that invalidated a regular inode's stat cache.
#[inline]
pub fn record_ext4_fstat_cache_invalidation(reason: Ext4FstatMissReason) {
    EXT4_FSTAT_CACHE_INVALIDATIONS.record(reason);
}

/// Record one actual `Ext4File::fstat()` that retained the default
/// `ColdInode` reason. This is intentionally not called for fast-cache hits,
/// invalidation-triggered misses, or alias-recovery retries.
#[inline]
pub fn record_ext4_fstat_cold_inode(kind: Ext4FstatColdInodeKind, has_lookup_stat: bool) {
    EXT4_FSTAT_COLD_INODE_COUNTS.record(kind, has_lookup_stat);
}

/// Record a discarded directory-stat snapshot.
///
/// The total counter keeps continuity with older logs, while the local/global
/// split shows whether misses came from this directory's own metadata or from
/// the conservative mount-wide namespace boundary.
#[inline]
pub fn record_ext4_fstat_directory_stat_epoch_miss(local_miss: bool, global_miss: bool) {
    add(&EXT4_FSTAT_DIRECTORY_STAT_EPOCH_MISSES, 1);
    if local_miss {
        add(&EXT4_FSTAT_DIRECTORY_STAT_LOCAL_EPOCH_MISSES, 1);
    }
    if global_miss {
        add(&EXT4_FSTAT_DIRECTORY_STAT_GLOBAL_EPOCH_MISSES, 1);
    }
}

#[inline]
pub fn record_ext4_fstat_directory_parent_local() {
    add(&EXT4_FSTAT_DIRECTORY_PARENT_LOCAL_UPDATES, 1);
}

#[inline]
pub fn record_ext4_fstat_directory_parent_global() {
    add(&EXT4_FSTAT_DIRECTORY_PARENT_GLOBAL_FALLBACKS, 1);
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

/// Sub-phases inside create/open-with-create while the namespace gate is held.
/// These counters explain which part of the broad create bucket justifies any
/// future lock-boundary change.
#[derive(Clone, Copy)]
pub enum Ext4CreatePhase {
    ExistCheck,
    DirMkOrFileOpen,
    FileClose,
    MetadataApply,
    VfsFinish,
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

/// Scope guard for an lwext4 or VFS phase. Drop-based accounting keeps failed
/// calls in the same aggregate as successful ones without per-call logging.
pub struct Ext4InodePhaseGuard {
    phase: Ext4InodePhase,
    begin: usize,
}

pub struct Ext4CreatePhaseGuard {
    phase: Ext4CreatePhase,
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

impl Ext4CreatePhaseGuard {
    #[inline]
    pub fn new(phase: Ext4CreatePhase) -> Self {
        Self {
            phase,
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

impl Drop for Ext4CreatePhaseGuard {
    #[inline]
    fn drop(&mut self) {
        let elapsed = get_ticks().saturating_sub(self.begin);
        match self.phase {
            Ext4CreatePhase::ExistCheck => EXT4_NAMESPACE_CREATE_EXIST_CHECK.record(elapsed),
            Ext4CreatePhase::DirMkOrFileOpen => EXT4_NAMESPACE_CREATE_NODE_OPEN.record(elapsed),
            Ext4CreatePhase::FileClose => EXT4_NAMESPACE_CREATE_FILE_CLOSE.record(elapsed),
            Ext4CreatePhase::MetadataApply => EXT4_NAMESPACE_CREATE_METADATA_APPLY.record(elapsed),
            Ext4CreatePhase::VfsFinish => EXT4_NAMESPACE_CREATE_VFS_FINISH.record(elapsed),
        }
    }
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

/// Count cold pages loaded but not retained because the global page-cache
/// capacity was already reserved by other entries.
#[inline]
pub fn record_file_cache_capacity_bypass(pages: usize) {
    add(&FILE_CACHE_CAPACITY_BYPASS_PAGES, pages);
}

#[inline]
pub fn record_file_cache_eviction() {
    add(&FILE_CACHE_EVICTIONS, 1);
}

#[inline]
pub fn record_file_cache_eviction_scan() {
    add(&FILE_CACHE_EVICTION_SCANS, 1);
}

#[inline]
pub fn record_file_cache_eviction_second_chance() {
    add(&FILE_CACHE_EVICTION_SECOND_CHANCES, 1);
}

#[inline]
pub fn record_file_cache_eviction_dirty_skip() {
    add(&FILE_CACHE_EVICTION_DIRTY_SKIPS, 1);
}

#[inline]
pub fn record_file_cache_eviction_in_use_skip() {
    add(&FILE_CACHE_EVICTION_IN_USE_SKIPS, 1);
}

/// Count deferred candidates that were reintroduced to the active CLOCK queue
/// for a bounded retry after their cooldown expired.
#[inline]
pub fn record_file_cache_eviction_deferred_retry(pages: usize) {
    add(&FILE_CACHE_EVICTION_DEFERRED_RETRY_PAGES, pages);
}

/// Count capacity misses that bypassed the active scan while all candidates
/// were deferred. This is distinct from `capacity_bypass_pages`, which counts
/// cold pages not retained by the cache regardless of the reason.
#[inline]
pub fn record_file_cache_eviction_cooldown_bypass() {
    add(&FILE_CACHE_EVICTION_COOLDOWN_BYPASSES, 1);
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
pub fn record_vfs_dentry_positive_insert() {
    add(&VFS_DENTRY_POSITIVE_INSERTS, 1);
}

#[inline]
pub fn record_vfs_dentry_negative_insert() {
    add(&VFS_DENTRY_NEGATIVE_INSERTS, 1);
}

#[inline]
pub fn record_vfs_dentry_invalidate(removed: bool) {
    add(&VFS_DENTRY_INVALIDATES, 1);
    if removed {
        add(&VFS_DENTRY_INVALIDATE_HITS, 1);
    }
}

#[inline]
pub fn record_vfs_dentry_clear() {
    add(&VFS_DENTRY_CLEAR_CALLS, 1);
}

#[inline]
pub fn record_vfs_dentry_parent_miss() {
    add(&VFS_DENTRY_PARENT_MISSES, 1);
}

#[inline]
pub fn record_vfs_dentry_lookup_bypass_flags() {
    add(&VFS_DENTRY_LOOKUP_BYPASS_FLAGS, 1);
}

#[inline]
pub fn record_vfs_path_index_hit() {
    add(&VFS_PATH_INDEX_HITS, 1);
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

/// Record a new canonical entry installed immediately after FsIndex reclaimed
/// an unused inode. This is distinct from the number of reclaimed entries:
/// most rebuilt inodes keep their lookup stat and never execute lwext4 fstat.
#[inline]
pub fn record_vfs_fsidx_rebuild() {
    add(&VFS_FSINDEX_REBUILDS, 1);
}

/// A collision between two lookup identities was resolved from the Ext4
/// identity epoch, avoiding a live `fstat()` probe.
#[inline]
pub fn record_vfs_fsidx_identity_epoch_hit() {
    add(&VFS_FSINDEX_IDENTITY_EPOCH_HITS, 1);
}

/// An identity collision crossed an unlink/rename epoch (or came from a
/// backend without an epoch proof), so FsIndex retained live validation.
#[inline]
pub fn record_vfs_fsidx_identity_live_probe() {
    add(&VFS_FSINDEX_IDENTITY_LIVE_PROBES, 1);
}

/// The live identity probe rejected a stale canonical inode and replaced it.
#[inline]
pub fn record_vfs_fsidx_identity_stale_replace() {
    add(&VFS_FSINDEX_IDENTITY_STALE_REPLACES, 1);
}

#[inline]
pub fn record_vfs_dentry_capacity_evict(entries: usize) {
    add(&VFS_DENTRY_CAPACITY_EVICTIONS, 1);
    add(&VFS_DENTRY_CAPACITY_EVICTED_ENTRIES, entries);
}

/// Align the Rust device counters with the C bcache post-mount epoch.
#[cfg(feature = "perf")]
pub(crate) fn enable_ext4_block_device_perf() {
    EXT4_BLOCK_DEVICE_PERF_ENABLED.store(false, Ordering::Relaxed);
    EXT4_BLOCK_DEVICE_STATS.reset();
    EXT4_BLOCK_DEVICE_PERF_ENABLED.store(true, Ordering::Release);
}

/// Start a fresh measurement epoch after lwext4 mount/recovery has finished.
#[cfg(feature = "perf")]
pub(crate) fn enable_ext4_resource_lock_perf() {
    EXT4_RESOURCE_LOCK_PERF_ENABLED.store(false, Ordering::Relaxed);
    EXT4_RESOURCE_LOCK_STATS.reset();
    EXT4_RESOURCE_LOCK_PERF_ENABLED.store(true, Ordering::Release);
}

#[inline]
#[cfg(feature = "perf")]
pub(crate) fn record_ext4_block_request_submit(kind: Ext4BlockRequestKind, bytes: usize) {
    if !EXT4_BLOCK_DEVICE_PERF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    add(&EXT4_BLOCK_DEVICE_STATS.submits, 1);
    add(&EXT4_BLOCK_DEVICE_STATS.bytes, bytes);
    let requests = match kind {
        Ext4BlockRequestKind::Read => &EXT4_BLOCK_DEVICE_STATS.read_requests,
        Ext4BlockRequestKind::Write => &EXT4_BLOCK_DEVICE_STATS.write_requests,
        Ext4BlockRequestKind::Flush => &EXT4_BLOCK_DEVICE_STATS.flush_requests,
    };
    add(requests, 1);
}

#[inline]
#[cfg(feature = "perf")]
pub(crate) fn record_ext4_block_request_acquired(wait_ticks: usize, contended: bool) {
    if !EXT4_BLOCK_DEVICE_PERF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    add(&EXT4_BLOCK_DEVICE_STATS.wait_ticks, wait_ticks);
    update_max(&EXT4_BLOCK_DEVICE_STATS.max_wait_ticks, wait_ticks);
    if contended {
        add(&EXT4_BLOCK_DEVICE_STATS.contended, 1);
    }
}

#[inline]
#[cfg(feature = "perf")]
pub(crate) fn record_ext4_block_request_queued(queue_depth: usize) {
    if !EXT4_BLOCK_DEVICE_PERF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    add(&EXT4_BLOCK_DEVICE_STATS.queued, 1);
    update_max(&EXT4_BLOCK_DEVICE_STATS.max_queue_depth, queue_depth);
}

#[inline]
#[cfg(feature = "perf")]
pub(crate) fn record_ext4_block_request_complete(service_ticks: usize, success: bool) {
    if !EXT4_BLOCK_DEVICE_PERF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    add(&EXT4_BLOCK_DEVICE_STATS.completed, 1);
    add(&EXT4_BLOCK_DEVICE_STATS.service_ticks, service_ticks);
    update_max(&EXT4_BLOCK_DEVICE_STATS.max_service_ticks, service_ticks);
    if !success {
        add(&EXT4_BLOCK_DEVICE_STATS.errors, 1);
    }
}

#[inline]
#[cfg(feature = "perf")]
pub(crate) fn record_ext4_resource_lock_acquired(wait_ticks: usize, contended: bool) {
    if !EXT4_RESOURCE_LOCK_PERF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    add(&EXT4_RESOURCE_LOCK_STATS.acquires, 1);
    add(&EXT4_RESOURCE_LOCK_STATS.wait_ticks, wait_ticks);
    update_max(&EXT4_RESOURCE_LOCK_STATS.max_wait_ticks, wait_ticks);
    if contended {
        add(&EXT4_RESOURCE_LOCK_STATS.contended, 1);
    }
}

#[inline]
#[cfg(feature = "perf")]
pub(crate) fn record_ext4_resource_lock_queued(queue_depth: usize) {
    if !EXT4_RESOURCE_LOCK_PERF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    add(&EXT4_RESOURCE_LOCK_STATS.queued, 1);
    update_max(&EXT4_RESOURCE_LOCK_STATS.max_queue_depth, queue_depth);
}

#[inline]
#[cfg(feature = "perf")]
pub(crate) fn record_ext4_resource_lock_released(hold_ticks: usize) {
    if !EXT4_RESOURCE_LOCK_PERF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    add(&EXT4_RESOURCE_LOCK_STATS.hold_ticks, hold_ticks);
    update_max(&EXT4_RESOURCE_LOCK_STATS.max_hold_ticks, hold_ticks);
}

#[inline]
#[cfg(feature = "perf")]
pub(crate) fn record_ext4_resource_registry(lookup_ticks: usize, created: bool) {
    if !EXT4_RESOURCE_LOCK_PERF_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    add(&EXT4_RESOURCE_LOCK_STATS.registry_lookups, 1);
    add(&EXT4_RESOURCE_LOCK_STATS.registry_ticks, lookup_ticks);
    update_max(&EXT4_RESOURCE_LOCK_STATS.max_registry_ticks, lookup_ticks);
    if created {
        add(&EXT4_RESOURCE_LOCK_STATS.registry_creates, 1);
    }
}
