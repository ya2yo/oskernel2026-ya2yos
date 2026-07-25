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
static SYSCALL_MM: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_FUTEX: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_SCHED_YIELD: AtomicUsize = AtomicUsize::new(0);

static SYSCALL_READ_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_READ_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_WRITE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_WRITE_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_WRITE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
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
static SYSCALL_PROCESS_CLONE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_CLONE_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_CLONE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_EXEC_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_EXEC_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_EXEC_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_WAIT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_WAIT_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_WAIT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_WAIT_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_WAIT_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
static SYSCALL_PROCESS_WAIT_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_ADDRESS_SPACE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_ADDRESS_SPACE_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_ADDRESS_SPACE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCFS_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCFS_TICKS: AtomicUsize = AtomicUsize::new(0);
static CLONE_PROCFS_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);

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
        // brk/mmap/mprotect/munmap/mremap
        214 | 215 | 216 | 222 | 226 => add(&SYSCALL_MM, 1),
        // clone/clone3/execve/wait4/waitid
        95 | 220 | 221 | 260 | 435 => add(&SYSCALL_PROCESS, 1),
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

/// Return whether a syscall needs a duration sample.  Blocking network and
/// stream I/O are sampled at the syscall boundary; other calls are counted
/// without taking an extra clock read.
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
            | 95
            | 202
            | 203
            | 206
            | 207
            | 211
            | 212
            | 220
            | 221
            | 242
            | 260
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
        220 => (
            &SYSCALL_PROCESS_CLONE_SAMPLES,
            &SYSCALL_PROCESS_CLONE_TICKS,
            &SYSCALL_PROCESS_CLONE_MAX_TICKS,
        ),
        221 => (
            &SYSCALL_PROCESS_EXEC_SAMPLES,
            &SYSCALL_PROCESS_EXEC_TICKS,
            &SYSCALL_PROCESS_EXEC_MAX_TICKS,
        ),
        95 | 260 => (
            &SYSCALL_PROCESS_WAIT_SAMPLES,
            &SYSCALL_PROCESS_WAIT_TICKS,
            &SYSCALL_PROCESS_WAIT_MAX_TICKS,
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

/// Profile synthetic /proc entry creation during a process clone.
#[inline]
pub fn record_clone_procfs_duration(elapsed: usize) {
    record_duration(
        &CLONE_PROCFS_SAMPLES,
        &CLONE_PROCFS_TICKS,
        &CLONE_PROCFS_MAX_TICKS,
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

/// Scope guard used by `sys_read` so error returns are included as well.
pub struct ReadActiveGuard {
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
        "[perf] t={}ms syscalls total={} read={} write={} open={} close={} stat={} mm={} process={} futex={} yield={}",
        now,
        SYSCALL_TOTAL.load(Ordering::Relaxed),
        SYSCALL_READ.load(Ordering::Relaxed),
        SYSCALL_WRITE.load(Ordering::Relaxed),
        SYSCALL_OPEN.load(Ordering::Relaxed),
        SYSCALL_CLOSE.load(Ordering::Relaxed),
        SYSCALL_STAT.load(Ordering::Relaxed),
        SYSCALL_MM.load(Ordering::Relaxed),
        SYSCALL_PROCESS.load(Ordering::Relaxed),
        SYSCALL_FUTEX.load(Ordering::Relaxed),
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
        "clone",
        &SYSCALL_PROCESS_CLONE_SAMPLES,
        &SYSCALL_PROCESS_CLONE_TICKS,
        &SYSCALL_PROCESS_CLONE_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "execve",
        &SYSCALL_PROCESS_EXEC_SAMPLES,
        &SYSCALL_PROCESS_EXEC_TICKS,
        &SYSCALL_PROCESS_EXEC_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "wait",
        &SYSCALL_PROCESS_WAIT_SAMPLES,
        &SYSCALL_PROCESS_WAIT_TICKS,
        &SYSCALL_PROCESS_WAIT_MAX_TICKS,
    );
    print!("[perf] syscall_duration ");
    emit_duration(
        "wait_active",
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
        "procfs",
        &CLONE_PROCFS_SAMPLES,
        &CLONE_PROCFS_TICKS,
        &CLONE_PROCFS_MAX_TICKS,
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
