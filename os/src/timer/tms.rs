//! POSIX `struct tms` — `times(2)` 系统调用返回结构
//!
//! 记录进程的 CPU 时间使用情况:
//! - `tms_utime` : 用户态 CPU 时间
//! - `tms_stime` : 内核态 CPU 时间
//! - `tms_cutime`: 所有已等待子进程的用户态 CPU 时间累计
//! - `tms_cstime`: 所有已等待子进程的内核态 CPU 时间累计

use super::timedata::TimeData;

#[repr(C)]
pub struct Tms {
    pub tms_utime: isize,
    pub tms_stime: isize,
    pub tms_cutime: isize,
    pub tms_cstime: isize,
}

impl Tms {
    pub fn new(time_data: &TimeData) -> Self {
        Self {
            tms_utime: time_data.utime,
            tms_stime: time_data.stime,
            tms_cutime: time_data.cutime,
            tms_cstime: time_data.cstime,
        }
    }
}