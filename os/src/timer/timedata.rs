//! 每线程 CPU 时间统计数据 (`TimeData`)
//!
//! 每个 `TaskControlBlock` 持有一个 `TimeData` 实例，用于记录该线程的
//! 用户态/内核态 CPU 时间 (毫秒级精度)。
//!
//! 字段说明:
//! - `utime`  : 用户态 CPU 时间累计 (ms)
//! - `stime`  : 内核态 CPU 时间累计 (ms)
//! - `cutime` : 已等待子进程的用户态 CPU 时间累计 (ms)
//! - `cstime` : 已等待子进程的内核态 CPU 时间累计 (ms)
//! - `lasttime`: 上次采样时刻 (开机毫秒)，用于增量更新

use crate::timer::get_time_ms;

#[derive(Clone)]
pub struct TimeData {
    pub utime: isize,
    pub stime: isize,
    pub cutime: isize,
    pub cstime: isize,
    pub lasttime: isize,
}

impl Default for TimeData {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeData {
    pub fn new() -> Self {
        let now = get_time_ms() as isize;
        Self {
            utime: 0,
            stime: 0,
            cutime: 0,
            cstime: 0,
            lasttime: now,
        }
    }

    /// 更新用户态 CPU 时间 (自上次采样以来)
    pub fn update_utime(&mut self) {
        let now = get_time_ms() as isize;
        self.utime += now - self.lasttime;
        self.lasttime = now;
    }

    /// 更新内核态 CPU 时间 (自上次采样以来)
    pub fn update_stime(&mut self) {
        let now = get_time_ms() as isize;
        self.stime += now - self.lasttime;
        self.lasttime = now;
    }

    /// 清零所有时间累计
    pub fn clear(&mut self) {
        let now = get_time_ms() as isize;
        self.utime = 0;
        self.stime = 0;
        self.cutime = 0;
        self.cstime = 0;
        self.lasttime = now;
    }
}
