//! POSIX `struct rusage` — `getrusage(2)` 返回的资源使用统计
//!
//! 记录进程及其子进程的资源使用情况。
//! 当前内核仅实际填充 `ru_utime` / `ru_stime` (CPU 时间)，
//! 其余字段保留为 0 以满足结构体大小兼容。

use super::timeval::TimeVal;

#[allow(unused)]
#[repr(C)]
pub struct Rusage {
    pub ru_utime: TimeVal,
    pub ru_stime: TimeVal,
    // ---- 以下字段当前未实现，保留为兼容占位 ----
    ru_maxrss: isize,
    ru_ixrss: isize,
    ru_idrss: isize,
    ru_isrss: isize,
    ru_minflt: isize,
    ru_majflt: isize,
    ru_nswap: isize,
    ru_inblock: isize,
    ru_oublock: isize,
    ru_msgsnd: isize,
    ru_msgrcv: isize,
    ru_nsignals: isize,
    ru_nvcsw: isize,
    ru_nivcsw: isize,
}

impl Rusage {
    /// 从毫秒精度的用户态/内核态时间构造 Rusage
    pub fn new_from_ms(utime: usize, stime: usize) -> Self {
        Self {
            ru_utime: TimeVal::new(utime / 1000, (utime % 1000) * 1000),
            ru_stime: TimeVal::new(stime / 1000, (stime % 1000) * 1000),
            ru_maxrss: 0,
            ru_ixrss: 0,
            ru_idrss: 0,
            ru_isrss: 0,
            ru_minflt: 0,
            ru_majflt: 0,
            ru_nswap: 0,
            ru_inblock: 0,
            ru_oublock: 0,
            ru_msgsnd: 0,
            ru_msgrcv: 0,
            ru_nsignals: 0,
            ru_nvcsw: 0,
            ru_nivcsw: 0,
        }
    }
}
