//! 通用时间管理模块
use core::ops::{Add, Sub};
use core::cmp::Ordering;
use core::sync::atomic::{AtomicU64, Ordering};
use alloc::{collections::BinaryHeap, sync::{Arc, Weak}};
use spin::{Lazy, Mutex};
use crate::sync::SyncUnsafeCell;
use crate::arch::time::{TimeImpl, TimeInterface}; // 引入上面的硬件实现
use crate::task::{handle_timer, TaskControlBlock};

// --- 常量定义 ---
pub const NANOS_PER_SEC: u64 = 1_000_000_000;
pub const NANOS_PER_MS: u64 = 1_000_000;
pub const NOW_TIME_STAMP: u64 = 1758325855; // 2025年基准时间戳

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timespec {
    pub tv_sec: usize,
    pub tv_nsec: usize,
}

impl Timespec {
    pub fn new(sec: usize, nsec: usize) -> Self {
        Self { tv_sec: sec, tv_nsec: nsec }
    }

    /// 将 Timespec 转换为总纳秒数
    pub fn as_nanos(&self) -> u64 {
        self.tv_sec as u64 * NANOS_PER_SEC + self.tv_nsec as u64
    }

    /// 从纳秒数创建 Timespec
    pub fn from_nanos(nanos: u64) -> Self {
        Self {
            tv_sec: (nanos / NANOS_PER_SEC) as usize,
            tv_nsec: (nanos % NANOS_PER_SEC) as usize,
        }
    }

    /// 转换为硬件 Ticks（用于底层比较）
    pub fn to_ticks(&self) -> u64 {
        TimeImpl::nanos_to_ticks(self.as_nanos())
    }
}

// 简化 Timespec 的算术运算
impl Add for Timespec {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::from_nanos(self.as_nanos() + rhs.as_nanos())
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeVal {
    pub tv_sec: usize,
    pub tv_usec: usize,
}

impl TimeVal {
    pub fn new(sec: usize, usec: usize) -> Self {
        Self { tv_sec: sec, tv_usec: usec }
    }
    
    pub fn now() -> Self {
        let nanos = monotonic_time_nanos();
        Self {
            tv_sec: (nanos / NANOS_PER_SEC) as usize,
            tv_usec: ((nanos % NANOS_PER_SEC) / 1000) as usize,
        }
    }
}

/// 获取自系统启动以来的纳秒数（单调递增）
pub fn monotonic_time_nanos() -> u64 {
    TimeImpl::ticks_to_nanos(TimeImpl::current_ticks())
}

pub fn get_time_ms() -> usize {
    (monotonic_time_nanos() / NANOS_PER_MS) as usize
}

pub fn get_time_spec() -> Timespec {
    Timespec::from_nanos(monotonic_time_nanos())
}

/// 设置下次触发时间（基于当前时间加 1 秒）
pub fn set_next_trigger() {
    let next_ns = monotonic_time_nanos() + NANOS_PER_SEC;
    TimeImpl::set_oneshot_timer(next_ns);
}

#[derive(Clone)]
pub struct TimeData {
    pub utime: usize,
    pub stime: usize,
    pub last_timestamp_ms: usize,
}

impl TimeData {
    pub fn new() -> Self {
        Self {
            utime: 0,
            stime: 0,
            last_timestamp_ms: get_time_ms(),
        }
    }

    fn update_common(&mut self) -> usize {
        let now = get_time_ms();
        let diff = now.saturating_sub(self.last_timestamp_ms);
        self.last_timestamp_ms = now;
        diff
    }

    pub fn update_utime(&mut self) { self.utime += self.update_common(); }
    pub fn update_stime(&mut self) { self.stime += self.update_common(); }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TimerType { Futex, StoppedTask }

pub struct TimerCondVar {
    pub expire: Timespec,
    pub task: Weak<TaskControlBlock>,
    pub kind: TimerType,
    pub extra_data: usize,
}

impl PartialEq for TimerCondVar {
    fn eq(&self, other: &Self) -> bool { self.expire == other.expire }
}
impl Eq for TimerCondVar {}

impl Ord for TimerCondVar {
    fn cmp(&self, other: &Self) -> Ordering {
        other.expire.cmp(&self.expire)
    }
}
impl PartialOrd for TimerCondVar {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub static TIMERS: Lazy<Mutex<BinaryHeap<TimerCondVar>>> =
    Lazy::new(|| Mutex::new(BinaryHeap::new()));

pub fn check_futex_timer() {
    let mut timers = TIMERS.lock();
    let current = get_time_spec();
    
    while let Some(timer) = timers.peek() {
        if timer.expire <= current {
            if let Some(task) = timer.task.upgrade() {
                if timer.kind == TimerType::Futex {
                    handle_timer(task, timer.extra_data);
                }
            }
            timers.pop();
        } else {
            break;
        }
    }
}

/// 系统启动时距离 Unix Epoch 的偏移量（纳秒）
static EPOCH_OFFSET_NANOS: AtomicU64 = AtomicU64::new(1758325855 * 1_000_000_000);

/// 获取开机后的单调时间（纳秒）
pub fn monotonic_time_nanos() -> u64 {
    TimeImpl::ticks_to_nanos(TimeImpl::current_ticks())
}

/// 获取实时时间（纳秒）
pub fn wall_time_nanos() -> u64 {
    // 实时时间 = 单调时间 + 偏移量
    monotonic_time_nanos() + EPOCH_OFFSET_NANOS.load(Ordering::Relaxed)
}


pub fn set_wall_time(now_nanos: u64) {
    let monotonic = monotonic_time_nanos();
    // 计算新的偏移量：偏移量 = 真实当前时间 - 单调时间
    let new_offset = now_nanos.saturating_sub(monotonic);
    EPOCH_OFFSET_NANOS.store(new_offset, Ordering::Relaxed);
}