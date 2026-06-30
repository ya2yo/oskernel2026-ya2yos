//! 系统时间管理模块
//!
//! 本模块管理内核中所有时间相关数据结构和 helper。当前 Ya2yOS 没有
//! Linux 那样完整的 `timekeeper`，因此需要明确区分不同时间语义，避免把
//! uptime、realtime、CPU time 和 timeout deadline 混用。
//!
//! # 当前内核需要维护的时间
//!
//! ## 1. 硬件计数时间
//!
//! - **来源**：架构层 `get_ticks()` 与 `get_clock_freq()`。
//! - **语义**：硬件 tick 单调递增，是所有软件时间的根。
//! - **用途**：换算出开机后经过的毫秒/纳秒，并驱动下一次 timer interrupt。
//! - **注意**：这不是 POSIX 可见时间，不能被 `settimeofday()` 修改。
//!
//! ## 2. 开机单调时间 / uptime
//!
//! - **接口**：`get_time_ms()`、`get_time_ns()`、`get_time_spec()`、
//!   `TimeVal::now()`。
//! - **语义**：从内核启动到现在经过的时间，单调递增，不包含 UNIX epoch。
//! - **用途**：
//!   - `CLOCK_MONOTONIC` / 相对睡眠 / `poll` / `select` / `epoll` timeout。
//!   - futex、sigtimedwait、stopped task 等内核阻塞超时 deadline。
//!   - `TimeData` 更新时作为 CPU time 采样基准。
//! - **注意**：所有相对 timeout 都应该优先使用 uptime，不能受用户调系统时间影响。
//!
//! ## 3. 墙上时间 / `CLOCK_REALTIME`
//!
//! - **接口**：`clock_gettime(CLOCK_REALTIME)`、`gettimeofday()`、文件时间戳等。
//! - **语义**：用户可见的 UNIX 时间，等价于 Linux 中已经被 timekeeper 调整后的
//!   `tk->xtime_sec` / `xtime_nsec`。
//! - **当前实现**：
//!   `CLOCK_REALTIME = uptime + NOW_TIME_STAMP + CLOCK_REALTIME_OFFSET`。
//! - **`NOW_TIME_STAMP`**：启动时的固定 UNIX epoch 基准。它把“开机 0 秒”
//!   转成一个近似墙上时间。
//! - **`CLOCK_REALTIME_OFFSET`**：Ya2yOS 的简化 timekeeper 偏移。Linux 没有
//!   这个同名变量，因为 Linux 会直接维护已调整的 wall time；本内核暂时用它记录
//!   `clock_settime(CLOCK_REALTIME)` / `settimeofday()` 造成的秒级修正。
//! - **注意**：realtime 可以跳变，不能拿它作为相对等待或 CPU 计时基准。
//!
//! ## 4. 子系统流逝时间 helper
//!
//! - **接口**：网络协议栈和异步 `TimerFuture` 直接使用 `get_time_ns()` /
//!   `get_time_spec()`。
//! - **语义**：这些子系统需要 elapsed time，不需要日历时间。
//! - **注意**：不要为了这类 timeout 引入 `NOW_TIME_STAMP` 或
//!   `CLOCK_REALTIME_OFFSET`，否则命名上容易和日历时间混淆，也可能被调时影响。
//!
//! ## 5. 线程 / 进程 CPU 时间
//!
//! - **结构**：`TimeData`、`Tms`、`Rusage`。
//! - **字段**：`utime`、`stime`、`cutime`、`cstime`，当前以毫秒累计。
//! - **语义**：任务实际消耗的用户态/内核态 CPU 时间，以及已 wait 子进程累计值。
//! - **用途**：`times(2)`、`getrusage(2)`、`wait4()`/`waitid()` rusage。
//! - **注意**：CPU time 不是墙上时间；阻塞等待期间不应累计为当前任务 CPU time。
//!
//! ## 6. 内核阻塞 deadline 定时器
//!
//! - **结构**：`TIMERS`、`TimerCondVar`、`TimerType`。
//! - **语义**：以 uptime `Timespec` 表示的绝对 deadline。
//! - **用途**：futex timeout、sigtimedwait timeout、stopped task timeout 等。
//! - **注意**：deadline 应该基于单调 uptime，避免 `settimeofday()` 导致等待时间跳变。
//!
//! ## 7. 每线程 interval timer
//!
//! - **结构**：`Timer`、`TimerInner`、`Itimerval`。
//! - **接口**：`setitimer(2)` / `getitimer(2)`。
//! - **语义**：每个线程持有一个 itimer，记录 interval、当前 value、last_time
//!   和一次性/周期性触发状态。
//! - **用途**：当前主要支持 `ITIMER_REAL`，到期后投递 SIGALRM，并用于 LTP timeout。
//!
//! ## 8. NTP / timex 状态
//!
//! - **结构**：`Timex`、`REALTIME_TIMEX`。
//! - **接口**：`adjtimex(2)` / `clock_adjtime(2)`。
//! - **语义**：保存用户设置的 NTP/timex 参数，向用户返回兼容结构。
//! - **注意**：当前实现主要是状态缓存和兼容返回，尚未实现 Linux 那种持续 slew
//!   或 frequency discipline；因此它不是驱动 `CLOCK_REALTIME` 前进的主时钟。
//!
//! # POSIX 时间结构
//!
//! - **`Timespec`**：`{ tv_sec, tv_nsec }`，纳秒精度，用于 `clock_gettime`、
//!   `clock_nanosleep` 等。
//! - **`TimeVal`**：`{ tv_sec, tv_usec }`，微秒精度，用于 `gettimeofday`、
//!   `setitimer`、`getrusage` 等旧接口。

mod itimerval;
mod rusage;
mod timedata;
mod timer_condvar;
mod timespec;
mod timeval;
mod timex;
mod tms;

pub use itimerval::{Itimerval, Timer, TimerInner, ITIMER_PROF, ITIMER_REAL, ITIMER_VIRTUAL};
pub use rusage::Rusage;
pub use timedata::TimeData;
pub use timer_condvar::{
    add_futex_timer, add_sigtimedwait_timer, add_stopped_task_timer, check_futex_timer,
    TimerCondVar, TimerType, TIMERS,
};
pub use timespec::Timespec;
pub use timeval::TimeVal;
pub use timex::{timex_apply, timex_get_realtime, Timex, TIME_OK};
pub use tms::Tms;

use crate::arch::time::{get_clock_freq, get_ticks, set_oneshot_timer};
use spin::{Lazy, Mutex};

//* 时间常量

/// 系统 tick 频率 (100Hz)，每 10ms 一个时钟滴答
const TICKS_PER_SEC: usize = 100;
/// 每秒钟的毫秒数
pub const MSEC_PER_SEC: usize = 1000;
/// 每秒钟的微秒数
pub const USEC_PER_SEC: u64 = 1_000_000;
/// 每秒钟的纳秒数
pub const NANOS_PER_SEC: u64 = 1_000_000_000;
/// 每毫秒的纳秒数
pub const NANOS_PER_MICROS: u64 = 1_000_000;
/// 开机时间到 UNIX 纪元 (1970-01-01) 的固定偏移量 (秒)
/// 2026-05-31 00:00:00 UTC
pub const NOW_TIME_STAMP: usize = 1_777_593_600;

// include/linux/posix-timer_types.h
pub const CPUCLOCK_PERTHREAD_MASK: i32 = 4;
pub const CPUCLOCK_CLOCK_MASK: i32 = 3;
pub const CPUCLOCK_PROF: i32 = 0;
pub const CPUCLOCK_VIRT: i32 = 1;
pub const CPUCLOCK_SCHED: i32 = 2;
pub const CPUCLOCK_MAX: i32 = 3;
pub const CLOCKFD: i32 = CPUCLOCK_MAX;
pub const CLOCKFD_MASK: i32 = CPUCLOCK_PERTHREAD_MASK | CPUCLOCK_CLOCK_MASK;

//* 墙上时钟偏移 (clock_settime / settimeofday 修改)

/// `clock_settime(CLOCK_REALTIME)` 对系统 REALTIME 的运行时偏移量 (秒)。
/// 初始为 0。通过 `clock_settime` / `settimeofday` 调整，在读取 REALTIME 时叠加。
pub static CLOCK_REALTIME_OFFSET: Lazy<Mutex<i64>> = Lazy::new(|| Mutex::new(0));

//* 时间获取函数

/// 获取自系统开机以来的毫秒数
pub fn get_time_ms() -> usize {
    get_ticks() / (get_clock_freq() / MSEC_PER_SEC)
}

/// 获取自系统开机以来的纳秒数
pub fn get_time_ns() -> usize {
    (get_ticks() as u128 * NANOS_PER_SEC as u128 / get_clock_freq() as u128) as usize
}

/// 获取当前日历时间纳秒数，即 POSIX `CLOCK_REALTIME`。
///
/// Linux 直接维护已经调时后的 timekeeper wall time；当前内核用
/// `NOW_TIME_STAMP + CLOCK_REALTIME_OFFSET` 叠加在 uptime 上模拟同一语义。
pub fn realtime_nanos() -> u64 {
    let nanos = get_time_ns() as i128
        + NOW_TIME_STAMP as i128 * NANOS_PER_SEC as i128
        + *CLOCK_REALTIME_OFFSET.lock() as i128 * NANOS_PER_SEC as i128;
    if nanos <= 0 {
        0
    } else if nanos > u64::MAX as i128 {
        u64::MAX
    } else {
        nanos as u64
    }
}

/// 获取当前日历时间 `Timespec`，即 POSIX `CLOCK_REALTIME`。
pub fn realtime() -> Timespec {
    Timespec::from_nanos(realtime_nanos())
}

/// 获取当前时间的 Timespec 表示 (开机时间，不含 NOW_TIME_STAMP)
pub fn get_time_spec() -> Timespec {
    let time = get_time_ms();
    Timespec::new(time / 1000, (time % 1000) * 1000000)
}

/// 设置下一次时钟中断 (每 10ms)
pub fn set_next_trigger() {
    set_oneshot_timer(get_ticks() + get_clock_freq() / TICKS_PER_SEC);
}

/// 计算从当前时间到 `endtime` 的剩余时间
pub fn calculate_left_timespec(endtime: Timespec) -> Timespec {
    let nowtime = get_time_spec();
    let mut endsec = endtime.tv_sec;
    let mut nsec: isize = endtime.tv_nsec as isize - nowtime.tv_nsec as isize;
    if nsec < 0 {
        endsec -= 1;
        nsec += 1_000_000_000isize;
    }
    Timespec {
        tv_sec: endsec - nowtime.tv_sec,
        tv_nsec: nsec as usize,
    }
}
