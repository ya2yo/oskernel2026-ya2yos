//! 系统时间管理模块
//!
//! 本模块管理 OS 内核中的所有时间相关数据结构和功能，按数据结构
//! 拆分为独立子文件。
//!
//! # 时间变量一览
//!
//! ## 硬件时钟层
//! - **`get_ticks()`** → RISC-V `mtime` 寄存器原始 tick 数，单调递增
//! - **`get_clock_freq()`** → CPU 时钟频率 (Hz)，如 10MHz
//!
//! ## 内核时间原语 (本模块)
//! - **`get_time_ms()`** → 自开机以来的毫秒数 (基于 tick 换算)
//! - **`get_time_ns()`** → 自开机以来的纳秒数
//! - **`get_time_spec()`** → 返回 `Timespec`，tv_sec = 开机秒数，tv_nsec = 亚秒纳秒
//!
//! ## 墙上时钟 (real-time clock)
//! - **`NOW_TIME_STAMP`** → 编译期硬编码的 UNIX 时间戳偏移 (1758325855 ≈ 2025-09)，加到
//!   开机时间上模拟真实墙上时钟
//! - **`CLOCK_REALTIME_OFFSET`** → 运行时通过 `clock_settime(2)` 调整的偏移量 (秒)，
//!   用于 NTP / adjtimex / clock_settime 对 REALTIME 的修正
//! - **`wall_time()`** / **`wall_time_nanos()`** → 开机时间 + NOW_TIME_STAMP = 模拟墙上时钟
//!
//! ## 定时器
//! - **`TICKS_PER_SEC`** → 系统 tick 频率 (100Hz)，即每 10ms 一次时钟中断
//! - **`set_next_trigger()`** → 设置 mtimecmp 寄存器触发下一次时钟中断
//! - **`TIMERS`** → 全局定时器堆 (`BinaryHeap<TimerCondVar>`)，用于 futex 超时等
//! - **`Timer` / `TimerInner`** → 每线程的 itimer (setitimer/getitimer)
//!
//! ## POSIX 时间结构体
//! - **`Timespec`** → `{tv_sec: usize, tv_nsec: usize}`，纳秒精度，用于 clock_gettime 等
//! - **`TimeVal`** → `{tv_sec: usize, tv_usec: usize}`，微秒精度，用于 gettimeofday 等旧接口
//!
//! ## 进程时间统计
//! - **`TimeData`** → 每线程的 CPU 时间统计 (utime/stime/cutime/cstime)
//! - **`Tms`** → `times(2)` 系统调用返回的格式
//! - **`Rusage`** → `getrusage(2)` 系统调用返回的资源使用统计
//!
//! ## NTP 时间调整
//! - **`Timex`** → `struct timex`，用于 adjtimex(2) / clock_adjtime(2)
//! - **`REALTIME_TIMEX`** → 全局 NTP 状态缓存
//! - **`timex_get_realtime()`** → 读取当前 timex (modes=0)
//! - **`timex_apply()`** → 应用 timex 调整 (modes≠0)

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

// ---------------------------------------------------------------------------
// 时间常量
// ---------------------------------------------------------------------------

/// 系统 tick 频率 (100Hz)，每 10ms 一个时钟滴答
const TICKS_PER_SEC: usize = 100;
/// 每秒钟的毫秒数
pub const MSEC_PER_SEC: usize = 1000;
/// 每秒钟的微秒数
pub const USEC_PER_SEC: u64 = 1_000_000;
/// 每秒钟的纳秒数
pub const NANOS_PER_SEC: u64 = 1_000_000_000;
/// 没毫秒的纳秒数
pub const NANOS_PER_MICROS: u64 = 1_000_000;
/// 开机时间到 UNIX 纪元 (1970-01-01) 的固定偏移量 (秒)
//// 2026-05-31 00:00:00 UTC
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


// 墙上时钟偏移 (clock_settime / adjtimex 修改)

/// `clock_settime(CLOCK_REALTIME)` 对系统 REALTIME 的运行时偏移量 (秒)。
/// 初始为 0。通过 `clock_settime` 调整，在 `clock_gettime(CLOCK_REALTIME)` 读取时叠加。
pub static CLOCK_REALTIME_OFFSET: Lazy<Mutex<i64>> = Lazy::new(|| Mutex::new(0));

// 时间获取函数

/// 获取自系统开机以来的毫秒数
pub fn get_time_ms() -> usize {
    get_ticks() / (get_clock_freq() / MSEC_PER_SEC)
}

/// 获取自系统开机以来的纳秒数
pub fn get_time_ns() -> usize {
    (get_ticks() as u128 * NANOS_PER_SEC as u128 / get_clock_freq() as u128) as usize
}

/// 获取当前的墙上时钟纳秒数 (开机时间 + NOW_TIME_STAMP)
pub fn wall_time_nanos() -> u64 {
    // get_time_ns() 单位是纳秒，NOW_TIME_STAMP 单位是秒，需要先把秒转换成纳秒再相加
    get_time_ns() as u64 + NOW_TIME_STAMP as u64 * NANOS_PER_SEC
}

/// 获取当前墙上时钟 Timespec (开机时间 + NOW_TIME_STAMP)
pub fn wall_time() -> Timespec {
    Timespec::from_nanos(wall_time_nanos())
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
