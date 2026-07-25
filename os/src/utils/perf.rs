//! Lightweight in-kernel counters used to diagnose long-running workloads.
//!
//! The counters are deliberately aggregate-only: hot paths perform relaxed
//! increments and the report is emitted at most once every 30 seconds.  This
//! keeps BuildStorm logs readable and avoids turning tracing itself into the
//! bottleneck we are trying to measure.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::time::{get_clock_freq, get_ticks};
use crate::timer::get_time_ms;

const REPORT_INTERVAL_MS: usize = 30_000;

static LAST_REPORT_MS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_TOTAL: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_WRITE: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_OPEN: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_CLOSE: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_STAT: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_LSEEK: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_MM: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_FUTEX: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_SCHED_YIELD: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_SIGACTION: AtomicUsize = AtomicUsize::new(0);

static SYSCALL_READ_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_WRITE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_WRITE_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_WRITE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_OPEN_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_OPEN_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_OPEN_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_CLOSE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_CLOSE_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_CLOSE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_STAT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_STAT_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_STAT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_LSEEK_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_LSEEK_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_LSEEK_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static LSEEK_IMPL_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static LSEEK_IMPL_TICKS: AtomicUsize = AtomicUsize::new(0);
static LSEEK_IMPL_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static LSEEK_TYPE_CHECK_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static LSEEK_TYPE_CHECK_TICKS: AtomicUsize = AtomicUsize::new(0);
static LSEEK_TYPE_CHECK_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static LSEEK_SIZE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static LSEEK_SIZE_TICKS: AtomicUsize = AtomicUsize::new(0);
static LSEEK_SIZE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static LSEEK_SPARSE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static LSEEK_SPARSE_TICKS: AtomicUsize = AtomicUsize::new(0);
static LSEEK_SPARSE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PATH_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PATH_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PATH_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_MM_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_MM_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_MM_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_FUTEX_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_FUTEX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_FUTEX_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_SIGACTION_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_SIGACTION_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_SIGACTION_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_CONNECT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_CONNECT_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_CONNECT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_ACCEPT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_ACCEPT_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_ACCEPT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_ACCEPT_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_ACCEPT_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_ACCEPT_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_SEND_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_SEND_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_SEND_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_RECV_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_RECV_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_NET_RECV_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_CLONE_TOTAL_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_CLONE_TOTAL_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_CLONE_TOTAL_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_EXEC_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_EXEC_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_EXEC_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_WAIT_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_WAIT_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_WAIT_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_ADDRESS_SPACE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_ADDRESS_SPACE_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_ADDRESS_SPACE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCESS_TOTAL_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCESS_TOTAL_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCESS_TOTAL_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_VFORK_WAIT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_VFORK_WAIT_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_VFORK_WAIT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_BOOTSTRAP_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_BOOTSTRAP_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_BOOTSTRAP_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PARENT_STATE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_PARENT_STATE_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PARENT_STATE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCESS_CREATE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCESS_CREATE_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCESS_CREATE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_TASK_SETUP_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_TASK_SETUP_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_TASK_SETUP_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCFS_REGISTER_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCFS_REGISTER_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCFS_REGISTER_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PUBLISH_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_PUBLISH_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PUBLISH_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_ENQUEUE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_ENQUEUE_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_ENQUEUE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static PROCFS_MATERIALIZE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static PROCFS_MATERIALIZE_TICKS: AtomicUsize = AtomicUsize::new(0);
static PROCFS_MATERIALIZE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);

static EXEC_IMAGE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static EXEC_IMAGE_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_IMAGE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_FROM_ELF_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static EXEC_FROM_ELF_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_FROM_ELF_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_STACK_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static EXEC_STACK_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_STACK_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_COMMIT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static EXEC_COMMIT_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_COMMIT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_KERNEL_SPACE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static EXEC_KERNEL_SPACE_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_KERNEL_SPACE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_INTERP_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static EXEC_INTERP_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_INTERP_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_INTERP_READ_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static EXEC_INTERP_READ_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_INTERP_READ_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_INTERP_MAP_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static EXEC_INTERP_MAP_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_INTERP_MAP_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_MAP_ELF_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static EXEC_MAP_ELF_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXEC_MAP_ELF_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);

static EXT4_READ_OPS: AtomicUsize = AtomicUsize::new(0);
static EXT4_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
static EXT4_LOCK_ACQUIRES: AtomicUsize = AtomicUsize::new(0);
static EXT4_LOCK_WAIT_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXT4_LOCK_HOLD_TICKS: AtomicUsize = AtomicUsize::new(0);

static FILE_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static FILE_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static FILE_PAGE_FAULTS: AtomicUsize = AtomicUsize::new(0);

static SCHEDULER_SELECTIONS: AtomicUsize = AtomicUsize::new(0);
static SCHEDULER_SELF_SELECTIONS: AtomicUsize = AtomicUsize::new(0);
static IDLE_LOOPS: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn add(counter: &AtomicUsize, value: usize) {
    counter.fetch_add(value, Ordering::Relaxed);
}

/// Count one syscall by the Linux ABI number.
#[inline]
pub fn record_syscall(id: usize) {
    add(&SYSCALL_TOTAL, 1);
    match id {
        // read/readv/pread* family
        63 | 65 | 67 | 69 | 286 => add(&SYSCALL_READ, 1),
        // write/writev/pwrite* family
        64 | 66 | 68 | 70 | 287 => add(&SYSCALL_WRITE, 1),
        // openat/openat2 and close/close_range
        56 | 437 => add(&SYSCALL_OPEN, 1),
        57 | 436 => add(&SYSCALL_CLOSE, 1),
        // fstat/statx and filesystem metadata probes
        43 | 44 | 79 | 80 | 88 | 291 => add(&SYSCALL_STAT, 1),
        // lseek
        62 => add(&SYSCALL_LSEEK, 1),
        // brk/mmap/mprotect/munmap/mremap
        214 | 215 | 216 | 222 | 226 => add(&SYSCALL_MM, 1),
        // clone/clone3/execve/wait4/waitid
        95 | 220 | 221 | 260 | 435 => add(&SYSCALL_PROCESS, 1),
        // signal
        134 => add(&SYSCALL_SIGACTION, 1),
        // futex and newer futex wait variants
        98 | 449 | 455 => add(&SYSCALL_FUTEX, 1),
        124 => add(&SYSCALL_SCHED_YIELD, 1),
        _ => {}
    }

    // Avoid reading the clock on every syscall.  BuildStorm reaches this
    // sampling point frequently, while small tests incur effectively no
    // extra work.
    let total = SYSCALL_TOTAL.load(Ordering::Relaxed);
    if total & 0xffff == 0 {
        maybe_report();
    }
}

/// Return whether a syscall needs a duration sample. BuildStorm's hot path is
/// dominated by process creation plus filesystem metadata and memory mapping,
/// so those categories are sampled at their syscall boundaries. Blocking
/// wait/futex operations are intentionally excluded: their duration is
/// recorded by active guards so descheduled time is not charged to the syscall.
#[inline]
pub fn should_record_syscall_duration(id: usize) -> bool {
    matches!(
        id,
        63 | 64
            | 65
            | 66
            | 67
            | 68
            | 69
            | 70
            | 33
            | 34
            | 35
            | 36
            | 37
            | 38
            | 45
            | 46
            | 47
            | 48
            | 49
            | 50
            | 52
            | 53
            | 54
            | 55
            | 56
            | 57
            | 61
            | 71
            | 78
            | 79
            | 80
            | 81
            | 82
            | 83
            | 84
            | 43
            | 44
            | 88
            | 62
            | 202
            | 203
            | 206
            | 207
            | 211
            | 212
            | 220
            | 435
            | 221
            | 214
            | 215
            | 216
            | 222
            | 226
            | 242
            | 276
            | 285
            | 291
            | 437
            | 436
            | 134
    )
}

#[inline]
fn update_max(counter: &AtomicUsize, value: usize) {
    let mut current = counter.load(Ordering::Relaxed);
    while value > current {
        match counter.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

#[inline]
fn record_duration(
    samples: &AtomicUsize,
    total: &AtomicUsize,
    maximum: &AtomicUsize,
    elapsed: usize,
) {
    add(samples, 1);
    add(total, elapsed);
    update_max(maximum, elapsed);
}

/// Add one sampled syscall duration to its aggregate bucket.
#[inline]
pub fn record_syscall_duration(id: usize, begin: usize) {
    if begin == 0 {
        return;
    }
    let elapsed = get_ticks().saturating_sub(begin);
    let (samples, total, maximum) = match id {
        63 | 65 | 67 | 69 => (
            &SYSCALL_READ_SAMPLES,
            &SYSCALL_READ_TICKS,
            &SYSCALL_READ_MAX_TICKS,
        ),
        64 | 66 | 68 | 70 => (
            &SYSCALL_WRITE_SAMPLES,
            &SYSCALL_WRITE_TICKS,
            &SYSCALL_WRITE_MAX_TICKS,
        ),
        56 | 437 => (
            &SYSCALL_OPEN_SAMPLES,
            &SYSCALL_OPEN_TICKS,
            &SYSCALL_OPEN_MAX_TICKS,
        ),
        57 | 436 => (
            &SYSCALL_CLOSE_SAMPLES,
            &SYSCALL_CLOSE_TICKS,
            &SYSCALL_CLOSE_MAX_TICKS,
        ),
        43 | 44 | 79 | 80 | 88 | 291 => (
            &SYSCALL_STAT_SAMPLES,
            &SYSCALL_STAT_TICKS,
            &SYSCALL_STAT_MAX_TICKS,
        ),
        62 => (
            &SYSCALL_LSEEK_SAMPLES,
            &SYSCALL_LSEEK_TICKS,
            &SYSCALL_LSEEK_MAX_TICKS,
        ),
        // Directory traversal, path mutation, and file-position/sync calls.
        33 | 34 | 35 | 36 | 37 | 38 | 45 | 46 | 47 | 48 | 49 | 50 | 52 | 53 | 54 | 55 | 61 | 71
        | 78 | 81 | 82 | 83 | 84 | 276 | 285 => (
            &SYSCALL_PATH_SAMPLES,
            &SYSCALL_PATH_TICKS,
            &SYSCALL_PATH_MAX_TICKS,
        ),
        214 | 215 | 216 | 222 | 226 => (
            &SYSCALL_MM_SAMPLES,
            &SYSCALL_MM_TICKS,
            &SYSCALL_MM_MAX_TICKS,
        ),
        134 => (
            &SYSCALL_SIGACTION_SAMPLES,
            &SYSCALL_SIGACTION_TICKS,
            &SYSCALL_SIGACTION_MAX_TICKS,
        ),
        202 | 242 => (
            &SYSCALL_NET_ACCEPT_SAMPLES,
            &SYSCALL_NET_ACCEPT_TICKS,
            &SYSCALL_NET_ACCEPT_MAX_TICKS,
        ),
        203 => (
            &SYSCALL_NET_CONNECT_SAMPLES,
            &SYSCALL_NET_CONNECT_TICKS,
            &SYSCALL_NET_CONNECT_MAX_TICKS,
        ),
        206 | 211 => (
            &SYSCALL_NET_SEND_SAMPLES,
            &SYSCALL_NET_SEND_TICKS,
            &SYSCALL_NET_SEND_MAX_TICKS,
        ),
        207 | 212 => (
            &SYSCALL_NET_RECV_SAMPLES,
            &SYSCALL_NET_RECV_TICKS,
            &SYSCALL_NET_RECV_MAX_TICKS,
        ),
        220 | 435 => (
            &SYSCALL_PROCESS_CLONE_TOTAL_SAMPLES,
            &SYSCALL_PROCESS_CLONE_TOTAL_TICKS,
            &SYSCALL_PROCESS_CLONE_TOTAL_MAX_TICKS,
        ),
        221 => (
            &SYSCALL_PROCESS_EXEC_SAMPLES,
            &SYSCALL_PROCESS_EXEC_TICKS,
            &SYSCALL_PROCESS_EXEC_MAX_TICKS,
        ),
        _ => return,
    };
    record_duration(samples, total, maximum, elapsed);
}

/// Profile the address-space portion of a process clone without emitting a
/// per-fork trace line.
#[inline]
pub fn record_clone_address_space_duration(elapsed: usize) {
    record_duration(
        &CLONE_ADDRESS_SPACE_SAMPLES,
        &CLONE_ADDRESS_SPACE_TICKS,
        &CLONE_ADDRESS_SPACE_MAX_TICKS,
        elapsed,
    );
}

/// Profile the full `TaskControlBlock::clone_process()` success path.
#[inline]
pub fn record_clone_process_total_duration(elapsed: usize) {
    record_duration(
        &CLONE_PROCESS_TOTAL_SAMPLES,
        &CLONE_PROCESS_TOTAL_TICKS,
        &CLONE_PROCESS_TOTAL_MAX_TICKS,
        elapsed,
    );
}

/// Profile clone work up to the child becoming runnable, excluding a possible
/// `CLONE_VFORK` parent wait after the child has been published.
#[inline]
pub fn record_clone_active_duration(elapsed: usize) {
    record_duration(
        &CLONE_ACTIVE_SAMPLES,
        &CLONE_ACTIVE_TICKS,
        &CLONE_ACTIVE_MAX_TICKS,
        elapsed,
    );
}

/// Profile the semantic parent wait imposed by `CLONE_VFORK`.
#[inline]
pub fn record_clone_vfork_wait_duration(elapsed: usize) {
    record_duration(
        &CLONE_VFORK_WAIT_SAMPLES,
        &CLONE_VFORK_WAIT_TICKS,
        &CLONE_VFORK_WAIT_MAX_TICKS,
        elapsed,
    );
}

/// Profile TID/kernel-stack allocation and the initial parent metadata snapshot.
#[inline]
pub fn record_clone_bootstrap_duration(elapsed: usize) {
    record_duration(
        &CLONE_BOOTSTRAP_SAMPLES,
        &CLONE_BOOTSTRAP_TICKS,
        &CLONE_BOOTSTRAP_MAX_TICKS,
        elapsed,
    );
}

/// Profile the parent task lock scope. `address_space` is a nested subphase.
#[inline]
pub fn record_clone_parent_state_duration(elapsed: usize) {
    record_duration(
        &CLONE_PARENT_STATE_SAMPLES,
        &CLONE_PARENT_STATE_TICKS,
        &CLONE_PARENT_STATE_MAX_TICKS,
        elapsed,
    );
}

/// Profile creation and registration of the child `Process` object.
#[inline]
pub fn record_clone_process_create_duration(elapsed: usize) {
    record_duration(
        &CLONE_PROCESS_CREATE_SAMPLES,
        &CLONE_PROCESS_CREATE_TICKS,
        &CLONE_PROCESS_CREATE_MAX_TICKS,
        elapsed,
    );
}

/// Profile child task construction, user resources, and child-visible state.
#[inline]
pub fn record_clone_task_setup_duration(elapsed: usize) {
    record_duration(
        &CLONE_TASK_SETUP_SAMPLES,
        &CLONE_TASK_SETUP_TICKS,
        &CLONE_TASK_SETUP_MAX_TICKS,
        elapsed,
    );
}

/// Profile in-memory `/proc` PID registration during a process clone.
#[inline]
pub fn record_clone_procfs_register_duration(elapsed: usize) {
    record_duration(
        &CLONE_PROCFS_REGISTER_SAMPLES,
        &CLONE_PROCFS_REGISTER_TICKS,
        &CLONE_PROCFS_REGISTER_MAX_TICKS,
        elapsed,
    );
}

/// Profile final child publication to task and shared-resource registries.
#[inline]
pub fn record_clone_publish_duration(elapsed: usize) {
    record_duration(
        &CLONE_PUBLISH_SAMPLES,
        &CLONE_PUBLISH_TICKS,
        &CLONE_PUBLISH_MAX_TICKS,
        elapsed,
    );
}

/// Profile placing the child into the scheduler ready queue.
#[inline]
pub fn record_clone_enqueue_duration(elapsed: usize) {
    record_duration(
        &CLONE_ENQUEUE_SAMPLES,
        &CLONE_ENQUEUE_TICKS,
        &CLONE_ENQUEUE_MAX_TICKS,
        elapsed,
    );
}

/// Profile a real deferred `/proc/<pid>` EXT4 directory materialization.
#[inline]
pub fn record_procfs_materialize_duration(elapsed: usize) {
    record_duration(
        &PROCFS_MATERIALIZE_SAMPLES,
        &PROCFS_MATERIALIZE_TICKS,
        &PROCFS_MATERIALIZE_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_image_duration(elapsed: usize) {
    record_duration(
        &EXEC_IMAGE_SAMPLES,
        &EXEC_IMAGE_TICKS,
        &EXEC_IMAGE_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_from_elf_duration(elapsed: usize) {
    record_duration(
        &EXEC_FROM_ELF_SAMPLES,
        &EXEC_FROM_ELF_TICKS,
        &EXEC_FROM_ELF_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_stack_duration(elapsed: usize) {
    record_duration(
        &EXEC_STACK_SAMPLES,
        &EXEC_STACK_TICKS,
        &EXEC_STACK_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_commit_duration(elapsed: usize) {
    record_duration(
        &EXEC_COMMIT_SAMPLES,
        &EXEC_COMMIT_TICKS,
        &EXEC_COMMIT_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_kernel_space_duration(elapsed: usize) {
    record_duration(
        &EXEC_KERNEL_SPACE_SAMPLES,
        &EXEC_KERNEL_SPACE_TICKS,
        &EXEC_KERNEL_SPACE_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_interp_duration(elapsed: usize) {
    record_duration(
        &EXEC_INTERP_SAMPLES,
        &EXEC_INTERP_TICKS,
        &EXEC_INTERP_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_interp_read_duration(elapsed: usize) {
    record_duration(
        &EXEC_INTERP_READ_SAMPLES,
        &EXEC_INTERP_READ_TICKS,
        &EXEC_INTERP_READ_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_interp_map_duration(elapsed: usize) {
    record_duration(
        &EXEC_INTERP_MAP_SAMPLES,
        &EXEC_INTERP_MAP_TICKS,
        &EXEC_INTERP_MAP_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_map_elf_duration(elapsed: usize) {
    record_duration(
        &EXEC_MAP_ELF_SAMPLES,
        &EXEC_MAP_ELF_TICKS,
        &EXEC_MAP_ELF_MAX_TICKS,
        elapsed,
    );
}

/// Record only the active accept poll closure, excluding time while the task
/// is descheduled waiting for a client connection.
#[inline]
pub fn record_accept_active_duration(elapsed: usize) {
    record_duration(
        &SYSCALL_NET_ACCEPT_ACTIVE_SAMPLES,
        &SYSCALL_NET_ACCEPT_ACTIVE_TICKS,
        &SYSCALL_NET_ACCEPT_ACTIVE_MAX_TICKS,
        elapsed,
    );
}

/// Record one active waitpid poll, excluding time while `block_on` deschedules
/// the task between two child-exit notifications.
#[inline]
pub fn record_wait_active_duration(elapsed: usize) {
    record_duration(
        &SYSCALL_PROCESS_WAIT_ACTIVE_SAMPLES,
        &SYSCALL_PROCESS_WAIT_ACTIVE_TICKS,
        &SYSCALL_PROCESS_WAIT_ACTIVE_MAX_TICKS,
        elapsed,
    );
}

/// Record one futex interval in which the task was actually executing.
///
/// A futex wait can leave the task descheduled between the pre-wait setup and
/// the wakeup path. Reusing the futex bucket for these active intervals keeps
/// the report compatible while removing that sleep time from the aggregate.
#[inline]
pub fn record_futex_active_duration(elapsed: usize) {
    record_duration(
        &SYSCALL_FUTEX_SAMPLES,
        &SYSCALL_FUTEX_TICKS,
        &SYSCALL_FUTEX_MAX_TICKS,
        elapsed,
    );
}

/// Record the non-blocking portion of a regular-file read syscall.
#[inline]
pub fn record_read_active_duration(elapsed: usize) {
    record_duration(
        &SYSCALL_READ_ACTIVE_SAMPLES,
        &SYSCALL_READ_ACTIVE_TICKS,
        &SYSCALL_READ_ACTIVE_MAX_TICKS,
        elapsed,
    );
}

#[inline]
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

/// Scope guard used by `sys_read` so error returns are included as well.
pub struct ReadActiveGuard {
    begin: usize,
}

/// Scope guard for clone setup work.  The caller drops it before a vfork
/// parent suspension so the active bucket does not include child execution.
pub struct CloneActiveGuard {
    begin: usize,
}

impl CloneActiveGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for CloneActiveGuard {
    #[inline]
    fn drop(&mut self) {
        record_clone_active_duration(get_ticks().saturating_sub(self.begin));
    }
}

impl ReadActiveGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for ReadActiveGuard {
    #[inline]
    fn drop(&mut self) {
        record_read_active_duration(get_ticks().saturating_sub(self.begin));
    }
}

/// A scope guard for measuring one active poll without touching every early
/// return in the poll closure.
pub struct WaitActiveGuard {
    begin: usize,
}

impl WaitActiveGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for WaitActiveGuard {
    #[inline]
    fn drop(&mut self) {
        record_wait_active_duration(get_ticks().saturating_sub(self.begin));
    }
}

/// Scope guard for one actively executing futex interval.
pub struct FutexActiveGuard {
    begin: usize,
}

impl FutexActiveGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for FutexActiveGuard {
    #[inline]
    fn drop(&mut self) {
        record_futex_active_duration(get_ticks().saturating_sub(self.begin));
    }
}

#[inline]
fn ticks_to_us(ticks: usize) -> usize {
    ticks
        .saturating_mul(1_000_000)
        .checked_div(get_clock_freq().max(1))
        .unwrap_or(usize::MAX)
}

fn emit_duration(label: &str, samples: &AtomicUsize, total: &AtomicUsize, maximum: &AtomicUsize) {
    println!(
        "{}(samples={} total_us={} max_us={})",
        label,
        samples.load(Ordering::Relaxed),
        ticks_to_us(total.load(Ordering::Relaxed)),
        ticks_to_us(maximum.load(Ordering::Relaxed)),
    );
}

#[inline]
pub fn record_ext4_read(bytes: usize) {
    add(&EXT4_READ_OPS, 1);
    add(&EXT4_READ_BYTES, bytes);
}

#[inline]
pub fn record_ext4_lock(wait_ticks: usize, hold_ticks: usize) {
    add(&EXT4_LOCK_ACQUIRES, 1);
    add(&EXT4_LOCK_WAIT_TICKS, wait_ticks);
    add(&EXT4_LOCK_HOLD_TICKS, hold_ticks);
    let samples = EXT4_LOCK_ACQUIRES.load(Ordering::Relaxed);
    if samples & 0x0fff == 0 {
        maybe_report();
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
pub fn record_file_page_fault() {
    add(&FILE_PAGE_FAULTS, 1);
}

#[inline]
pub fn record_scheduler_selection(self_selected: bool) {
    add(&SCHEDULER_SELECTIONS, 1);
    if self_selected {
        add(&SCHEDULER_SELF_SELECTIONS, 1);
    }
    let selections = SCHEDULER_SELECTIONS.load(Ordering::Relaxed);
    if selections & 0x0fff == 0 {
        maybe_report();
    }
}

#[inline]
pub fn record_idle_loop() {
    add(&IDLE_LOOPS, 1);
}

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

    emit_report(now);
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
    emit_report(now);
}

fn emit_report(now: usize) {
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
    println!(
        "[perf] ext4 reads={} bytes={} lock={} wait_ticks={} hold_ticks={} file_cache hit={} miss={} page_faults={}",
        EXT4_READ_OPS.load(Ordering::Relaxed),
        EXT4_READ_BYTES.load(Ordering::Relaxed),
        EXT4_LOCK_ACQUIRES.load(Ordering::Relaxed),
        EXT4_LOCK_WAIT_TICKS.load(Ordering::Relaxed),
        EXT4_LOCK_HOLD_TICKS.load(Ordering::Relaxed),
        FILE_CACHE_HITS.load(Ordering::Relaxed),
        FILE_CACHE_MISSES.load(Ordering::Relaxed),
        FILE_PAGE_FAULTS.load(Ordering::Relaxed),
    );
    println!(
        "[perf] scheduler selections={} self_selections={} idle_loops={}",
        SCHEDULER_SELECTIONS.load(Ordering::Relaxed),
        SCHEDULER_SELF_SELECTIONS.load(Ordering::Relaxed),
        IDLE_LOOPS.load(Ordering::Relaxed),
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
}
