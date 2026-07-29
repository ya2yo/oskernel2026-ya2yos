//! Syscall-boundary performance counters and active regular-file I/O scopes.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::time::get_ticks;

use super::common::{add, record_duration};
use super::maybe_report;

pub(crate) static SYSCALL_TOTAL: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_READ: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_WRITE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_OPEN: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_CLOSE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_STAT: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_LSEEK: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_MM: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_FUTEX: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_SCHED_YIELD: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_SIGACTION: AtomicUsize = AtomicUsize::new(0);

pub(crate) static SYSCALL_READ_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_READ_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_READ_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_READ_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_READ_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_READ_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_WRITE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_WRITE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_WRITE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_WRITE_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_WRITE_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_WRITE_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_OPEN_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_OPEN_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_OPEN_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_CLOSE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_CLOSE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_CLOSE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_STAT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_STAT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_STAT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_LSEEK_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_LSEEK_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_LSEEK_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_IMPL_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_IMPL_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_IMPL_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_TYPE_CHECK_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_TYPE_CHECK_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_TYPE_CHECK_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_SIZE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_SIZE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_SIZE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_SPARSE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_SPARSE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LSEEK_SPARSE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PATH_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PATH_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PATH_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_MM_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_MM_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_MM_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_FUTEX_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_FUTEX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_FUTEX_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_SIGACTION_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_SIGACTION_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_SIGACTION_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_CONNECT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_CONNECT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_CONNECT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_ACCEPT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_ACCEPT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_ACCEPT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_ACCEPT_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_ACCEPT_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_ACCEPT_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_SEND_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_SEND_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_SEND_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_RECV_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_RECV_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_NET_RECV_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS_CLONE_TOTAL_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS_CLONE_TOTAL_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS_CLONE_TOTAL_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS_EXEC_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS_EXEC_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS_EXEC_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS_WAIT_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS_WAIT_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SYSCALL_PROCESS_WAIT_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
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

/// Record the regular-file portion of a write syscall separately from pipe,
/// socket, and device writes that can spend most of their time blocked.
#[inline]
pub fn record_write_active_duration(elapsed: usize) {
    record_duration(
        &SYSCALL_WRITE_ACTIVE_SAMPLES,
        &SYSCALL_WRITE_ACTIVE_TICKS,
        &SYSCALL_WRITE_ACTIVE_MAX_TICKS,
        elapsed,
    );
}
pub struct ReadActiveGuard {
    begin: usize,
}

/// Scope guard used by `sys_write` for regular files so the aggregate write
/// duration can be attributed independently from non-regular fd waits.
pub struct WriteActiveGuard {
    begin: usize,
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

impl WriteActiveGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for WriteActiveGuard {
    #[inline]
    fn drop(&mut self) {
        record_write_active_duration(get_ticks().saturating_sub(self.begin));
    }
}
