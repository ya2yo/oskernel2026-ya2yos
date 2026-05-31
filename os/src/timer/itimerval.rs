//! POSIX `struct itimerval` 与 per-thread `Timer`
//!
//! # itimer 类型常量
//! - **`ITIMER_REAL`** (0): 墙上时钟倒计时，到期发送 SIGALRM
//! - **`ITIMER_VIRTUAL`** (1): 用户态 CPU 时间倒计时，到期发送 SIGVTALRM
//! - **`ITIMER_PROF`** (2): 总 CPU 时间倒计时，到期发送 SIGPROF
//!
//! 当前内核仅实际使用 `ITIMER_REAL`，其余为兼容预留。
//!
//! # `Itimerval` 结构
//! - `it_interval`: 周期性定时器的间隔 (0 表示单次)
//! - `it_value`: 首次到期时间 (0 表示禁用)
//!
//! # `Timer` / `TimerInner`
//! 每个线程持有一个 `Timer` (通过 `Arc<Timer>`)，用于 `setitimer(2)`/`getitimer(2)`:
//! - `timer`: 当前 itimerval 值
//! - `last_time`: 上次设置定时器时的墙上时钟，用于计算剩余时间
//! - `once`: 是否单次触发

use crate::sync::SyncUnsafeCell;
use super::timeval::TimeVal;

/// 以实际（挂钟）时间倒计时，到期发送 SIGALRM
pub const ITIMER_REAL: usize = 0;
/// 以用户态 CPU 时间倒计时，到期发送 SIGVTALRM
pub const ITIMER_VIRTUAL: usize = 1;
/// 以总 CPU 时间倒计时，到期发送 SIGPROF
pub const ITIMER_PROF: usize = 2;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Itimerval {
    pub it_interval: TimeVal,
    pub it_value: TimeVal,
}

impl Default for Itimerval {
    fn default() -> Self { Self::new() }
}

impl Itimerval {
    pub fn new() -> Self {
        Self {
            it_interval: TimeVal::new(0, 0),
            it_value: TimeVal::new(0, 0),
        }
    }
}

/// per-thread itimer 管理结构
pub struct Timer {
    pub inner: SyncUnsafeCell<TimerInner>,
}

pub struct TimerInner {
    pub timer: Itimerval,
    pub last_time: TimeVal,
    pub once: bool,
}

impl Default for TimerInner {
    fn default() -> Self { Self::new() }
}

impl TimerInner {
    pub fn new() -> Self {
        Self {
            timer: Itimerval::new(),
            last_time: TimeVal::new(0, 0),
            once: false,
        }
    }
}

impl Default for Timer {
    fn default() -> Self { Self::new() }
}

impl Timer {
    pub fn new() -> Self {
        Self {
            inner: SyncUnsafeCell::new(TimerInner::new()),
        }
    }

    pub fn set_timer(&self, new: Itimerval) {
        let inner = self.inner.get_unchecked_mut();
        inner.timer = new;
        inner.once = false;
        inner.last_time = TimeVal::new(0, 0);
    }

    pub fn set_last_time(&self, last_time: TimeVal) {
        self.inner.get_unchecked_mut().last_time = last_time;
    }

    pub fn set_trigger_once(&self, once: bool) {
        self.inner.get_unchecked_mut().once = once;
    }

    pub fn trigger_once(&self) -> bool {
        self.inner.get_unchecked_ref().once
    }

    pub fn last_time(&self) -> TimeVal {
        self.inner.get_unchecked_ref().last_time
    }

    pub fn timer(&self) -> Itimerval {
        self.inner.get_unchecked_ref().timer
    }
}