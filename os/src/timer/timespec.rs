//! POSIX `struct timespec` — 纳秒精度时间戳
//!
//! 用于:
//! - `clock_gettime(2)` / `clock_settime(2)` / `clock_nanosleep(2)`
//! - `nanosleep(2)` / `pselect6(2)` / `ppoll(2)` 等
//!
//! 字段: `tv_sec` (秒) + `tv_nsec` (纳秒, 0..10^9)
//!
//! 注意: 本内核的 `tv_sec`/`tv_nsec` 均为 `usize` (无符号)，
//! 与 Linux 的 `time_t` (有符号) 不同。负值检查应在系统调用层完成。

use core::cmp::Ordering;
use core::ops::Add;
use core::time::Duration;

use super::NANOS_PER_SEC;
use crate::arch::time::get_clock_freq;

const NSEC_PER_SEC: usize = 1_000_000_000;
const MSEC_PER_SEC: usize = 1_000;

#[repr(C)]
#[derive(Default, Debug, Ord, Clone, Copy, PartialEq, Eq)]
pub struct Timespec {
    pub tv_sec: usize,
    pub tv_nsec: usize,
}

// ---- 与标准库 Duration 互转 ----

impl From<Timespec> for Duration {
    fn from(ts: Timespec) -> Self {
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }
}

impl From<Duration> for Timespec {
    fn from(duration: Duration) -> Self {
        Self {
            tv_sec: duration.as_secs() as usize,
            tv_nsec: duration.subsec_nanos() as usize,
        }
    }
}

impl Add<Duration> for Timespec {
    type Output = Timespec;
    fn add(self, rhs: Duration) -> Self::Output {
        let mut sec = self.tv_sec + rhs.as_secs() as usize;
        let mut nsec = self.tv_nsec + rhs.subsec_nanos() as usize;
        if nsec >= 1_000_000_000 {
            sec += 1;
            nsec -= 1_000_000_000;
        }
        Timespec::new(sec, nsec)
    }
}

// ---- 构造与转换 ----

impl Timespec {
    pub fn new(sec: usize, nsec: usize) -> Self {
        Self {
            tv_sec: sec,
            tv_nsec: nsec,
        }
    }

    /// 转为 tick 数 (用于定时器堆比较)
    pub fn to_tick(&self) -> usize {
        let clock_freq = get_clock_freq();
        self.tv_sec * clock_freq + (self.tv_nsec * clock_freq / NSEC_PER_SEC)
    }

    /// 从纳秒数构造
    pub fn from_nanos(nanos: u64) -> Self {
        Self {
            tv_sec: (nanos / NANOS_PER_SEC) as usize,
            tv_nsec: (nanos % NANOS_PER_SEC) as usize,
        }
    }

    /// 从微秒数构造
    pub fn from_micros(micros: u64) -> Self {
        Self {
            tv_sec: (micros / MSEC_PER_SEC as u64) as usize,
            tv_nsec: (micros % MSEC_PER_SEC as u64) as usize,
        }
    }
}

// ---- 算术与比较 ----

impl Add for Timespec {
    type Output = Timespec;
    fn add(self, rhs: Self) -> Self::Output {
        let mut tv_sec = self.tv_sec + rhs.tv_sec;
        let mut tv_nsec = self.tv_nsec + rhs.tv_nsec;
        tv_sec += tv_nsec / 1_000_000_000;
        tv_nsec %= 1_000_000_000;
        Self { tv_sec, tv_nsec }
    }
}

impl PartialOrd for Timespec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(
            self.tv_sec
                .cmp(&other.tv_sec)
                .then_with(|| self.tv_nsec.cmp(&other.tv_nsec)),
        )
    }
}
