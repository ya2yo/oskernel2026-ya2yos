//! POSIX `struct timeval` — 微秒精度时间戳 (旧接口)
//!
//! 用于:
//! - `gettimeofday(2)` / `settimeofday(2)`
//! - `setitimer(2)` / `getitimer(2)` 的定时器值
//! - `rusage` 中的 CPU 时间字段
//!
//! 字段: `tv_sec` (秒) + `tv_usec` (微秒, 0..10^6)

use core::cmp::Ordering;
use core::ops::Add;

use crate::timer::get_time_ms;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeVal {
    pub tv_sec: usize,
    pub tv_usec: usize,
}

impl TimeVal {
    pub fn new(sec: usize, usec: usize) -> Self {
        Self {
            tv_sec: sec,
            tv_usec: usec,
        }
    }

    /// 返回表示当前开机时间的 TimeVal
    pub fn now() -> Self {
        let now_time = get_time_ms();
        Self {
            tv_sec: now_time / 1000,
            tv_usec: (now_time % 1000) * 1000,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tv_sec == 0 && self.tv_usec == 0
    }
}

impl Add for TimeVal {
    type Output = TimeVal;
    fn add(self, rhs: Self) -> Self::Output {
        let usec = self.tv_usec + rhs.tv_usec;
        Self {
            tv_sec: self.tv_sec + rhs.tv_sec + usec / 1_000_000,
            tv_usec: usec % 1_000_000,
        }
    }
}

impl PartialOrd for TimeVal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(
            self.tv_sec
                .cmp(&other.tv_sec)
                .then_with(|| self.tv_usec.cmp(&other.tv_usec)),
        )
    }
}
