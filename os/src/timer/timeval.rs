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

const USEC_PER_SEC: usize = 1_000_000;

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

    pub fn saturating_sub(self, rhs: Self) -> Self {
        if self <= rhs {
            return Self::new(0, 0);
        }

        if self.tv_usec >= rhs.tv_usec {
            Self::new(self.tv_sec - rhs.tv_sec, self.tv_usec - rhs.tv_usec)
        } else {
            Self::new(
                self.tv_sec - rhs.tv_sec - 1,
                self.tv_usec + USEC_PER_SEC - rhs.tv_usec,
            )
        }
    }
}

impl Add for TimeVal {
    type Output = TimeVal;
    fn add(self, rhs: Self) -> Self::Output {
        let usec = self.tv_usec + rhs.tv_usec;
        Self {
            tv_sec: self.tv_sec + rhs.tv_sec + usec / USEC_PER_SEC,
            tv_usec: usec % USEC_PER_SEC,
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
