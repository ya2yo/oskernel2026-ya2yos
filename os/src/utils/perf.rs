//! Lightweight in-kernel counters used to diagnose long-running workloads.
//!
//! The counters are deliberately aggregate-only: hot paths perform relaxed
//! increments and the report is emitted at most once every 30 seconds.  This
//! keeps BuildStorm logs readable and avoids turning tracing itself into the
//! bottleneck we are trying to measure.

use core::sync::atomic::{AtomicUsize, Ordering};

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

static EXT4_READ_OPS: AtomicUsize = AtomicUsize::new(0);
static EXT4_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
static EXT4_LOCK_ACQUIRES: AtomicUsize = AtomicUsize::new(0);
static EXT4_LOCK_WAIT_TICKS: AtomicUsize = AtomicUsize::new(0);
static EXT4_LOCK_HOLD_TICKS: AtomicUsize = AtomicUsize::new(0);

static FILE_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static FILE_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static FILE_PAGE_FAULTS: AtomicUsize = AtomicUsize::new(0);

static SCHEDULER_SELECTIONS: AtomicUsize = AtomicUsize::new(0);
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
pub fn record_scheduler_selection() {
    add(&SCHEDULER_SELECTIONS, 1);
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
        "[perf] scheduler selections={} idle_loops={}",
        SCHEDULER_SELECTIONS.load(Ordering::Relaxed),
        IDLE_LOOPS.load(Ordering::Relaxed),
    );
}
