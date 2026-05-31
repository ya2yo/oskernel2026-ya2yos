//! RISC-V timer-related functionality

use core::ops::Add;
use core::time::Duration;

use crate::arch::time::{get_ticks, set_oneshot_timer};
use crate::sync::SyncUnsafeCell;
use crate::task::{handle_timer, TaskControlBlock};
use crate::{arch::time::get_clock_freq, task::wakeup_futex_task};
use alloc::{
    collections::BinaryHeap,
    sync::{Arc, Weak},
};
use core::cmp::Ordering;
use log::debug;

use spin::{Lazy, Mutex};

const TICKS_PER_SEC: usize = 100;
const MSEC_PER_SEC: usize = 1000;
pub const NANOS_PER_SEC: u64 = 1_000_000_000;
pub const NANOS_PER_MICROS: u64 = 1_000;
pub const NOW_TIME_STAMP: usize = 1758325855; // add bu tuji :   1758325855 是2025年9月某时间的时间戳

/// clock_settime(CLOCK_REALTIME) 对系统时间的偏移量 (秒)
/// 初始为 0，通过 clock_settime 调整
pub static CLOCK_REALTIME_OFFSET: Lazy<Mutex<i64>> = Lazy::new(|| Mutex::new(0));

#[allow(unused)]
const USEC_PER_SEC: usize = 1000000;
const NSEC_PER_SEC: usize = 1000000000;

/// 遵循POSIX标准，用于高精度的时间戳
#[derive(Debug, Ord, Clone, Copy, PartialEq, Eq)]
pub struct Timespec {
    pub tv_sec: usize,  //秒
    pub tv_nsec: usize, //纳秒
}

impl From<Timespec> for Duration {
    fn from(ts: Timespec) -> Self {
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }
}
impl From<core::time::Duration> for Timespec {
    fn from(duration: core::time::Duration) -> Self {
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

        // 处理纳秒进位
        if nsec >= 1_000_000_000 {
            sec += 1;
            nsec -= 1_000_000_000;
        }

        Timespec::new(sec, nsec)
    }
}

impl Timespec {
    pub fn new(sec: usize, nsec: usize) -> Self {
        Self {
            tv_sec: sec,
            tv_nsec: nsec,
        }
    }
    pub fn to_tick(&self) -> usize {
        let clock_freq = get_clock_freq();
        self.tv_sec * clock_freq + (self.tv_nsec * clock_freq / NSEC_PER_SEC)
    }
    pub fn from_nanos(nanos: u64) -> Self {
        Self {
            tv_sec: (nanos / NANOS_PER_SEC) as usize,
            tv_nsec: (nanos % NANOS_PER_SEC) as usize,
        }
    }
    pub fn from_micros(micros: u64) -> Self {
        Self {
            tv_sec: (micros / MSEC_PER_SEC as u64) as usize,
            tv_nsec: (micros % MSEC_PER_SEC as u64) as usize,
        }
    }
}

impl Add for Timespec {
    type Output = Timespec;
    fn add(self, rhs: Self) -> Self::Output {
        let mut tv_sec = self.tv_sec + rhs.tv_sec;
        let mut tv_nsec = self.tv_nsec + rhs.tv_nsec;
        tv_sec += tv_nsec / (1_000_000_000usize);
        tv_nsec %= 1_000_000_000usize;
        Self { tv_sec, tv_nsec }
    }
}

impl PartialOrd for Timespec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.tv_sec > other.tv_sec {
            Some(Ordering::Greater)
        } else if self.tv_sec < other.tv_sec {
            Some(Ordering::Less)
        } else {
            if self.tv_nsec > other.tv_nsec {
                Some(Ordering::Greater)
            } else if self.tv_nsec < other.tv_nsec {
                Some(Ordering::Less)
            } else {
                Some(Ordering::Equal)
            }
        }
    }
}

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

pub struct Tms {
    pub tms_utime: isize,  //用户模式下花费的CPU时间
    pub tms_stime: isize,  //内核模式下花费的CPU时间
    pub tms_cutime: isize, //子进程在用户模式下花费的CPU时间
    pub tms_cstime: isize, //子进程在内核模式下花费的CPU时间
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
        let now = (get_time_ms()) as isize;
        Self {
            utime: 0,
            stime: 0,
            cutime: 0,
            cstime: 0,
            lasttime: now,
        }
    }
    pub fn update_utime(&mut self) {
        let now = (get_time_ms()) as isize;
        let duration = now - self.lasttime;
        self.utime += duration;
        self.lasttime = now;
    }
    pub fn update_stime(&mut self) {
        let now = (get_time_ms()) as isize;
        let duration = now - self.lasttime;
        self.stime += duration;
        self.lasttime = now;
    }
    pub fn clear(&mut self) {
        let now = (get_time_ms()) as isize;
        self.utime = 0;
        self.stime = 0;
        self.cutime = 0;
        self.cstime = 0;
        self.lasttime = now;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Itimerval {
    /// Interval for periodic timer
    pub it_interval: TimeVal,
    /// Time until next expiration
    pub it_value: TimeVal,
}

impl Default for Itimerval {
    fn default() -> Self {
        Self::new()
    }
}

impl Itimerval {
    pub fn new() -> Self {
        Self {
            it_interval: TimeVal::new(0, 0),
            it_value: TimeVal::new(0, 0),
        }
    }
}
///以实际（即挂钟）时间倒计时。在每次到期时，都会生成一个 SIGALRM 信号
pub const ITIMER_REAL: usize = 0;
/// 此计时器根据进程消耗的用户模式 CPU 时间倒计时。（测量值包括进程中所有线程消耗的 CPU 时间。
/// 在每次到期时，都会生成一个 SIGVTALRM 信号
pub const ITIMER_VIRTUAL: usize = 1;
/// 此计时器根据进程消耗的总 CPU 时间（即用户和系统）进行倒计时。（测量值包括进程中所有线程消耗的 CPU 时间。
/// 在每次到期时，都会生成一个 SIGPROF 信号。
pub const ITIMER_PROF: usize = 2;

/// 三种 itimer,实际只会使用ITIMER_REAL
pub struct Timer {
    pub inner: SyncUnsafeCell<TimerInner>,
}

pub struct TimerInner {
    pub timer: Itimerval,
    pub last_time: TimeVal,
    pub once: bool,
}

impl Default for TimerInner {
    fn default() -> Self {
        Self::new()
    }
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
    fn default() -> Self {
        Self::new()
    }
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
/// 遵循旧版POSIX标准，用于旧接口
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeVal {
    pub tv_sec: usize,  //秒
    pub tv_usec: usize, //微秒
}

impl TimeVal {
    pub fn new(sec: usize, usec: usize) -> Self {
        Self {
            tv_sec: sec,
            tv_usec: usec,
        }
    }
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
        if self.tv_sec > other.tv_sec {
            Some(Ordering::Greater)
        } else if self.tv_sec < other.tv_sec {
            Some(Ordering::Less)
        } else {
            if self.tv_usec > other.tv_usec {
                Some(Ordering::Greater)
            } else if self.tv_usec < other.tv_usec {
                Some(Ordering::Less)
            } else {
                Some(Ordering::Equal)
            }
        }
    }
}

/// NTP 时间调整参数（对应 Linux struct timex）
///
/// 布局与 RISC-V 64-bit 的 C struct timex 兼容。
/// 关键字段: modes(0)、offset(8)、freq(16)、status(40)、tick(88)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Timex {
    pub modes: u32,          // 0x00: 模式选择器
    _pad0: u32,              // 0x04: 对齐填充
    pub offset: i64,         // 0x08: 时间偏移 (usec)
    pub freq: i64,           // 0x10: 频率偏移 (scaled ppm)
    pub maxerror: i64,       // 0x18: 最大误差 (usec)
    pub esterror: i64,       // 0x20: 估计误差 (usec)
    pub status: i32,         // 0x28: 时钟状态
    _pad1: u32,              // 0x2c: 对齐填充
    pub constant: i64,       // 0x30: PLL 时间常数
    pub precision: i64,      // 0x38: 时钟精度 (usec, 只读)
    pub tolerance: i64,      // 0x40: 时钟频率容差 (ppm, 只读)
    pub time: TimeVal,       // 0x48: 当前时间 (只读)
    pub tick: i64,           // 0x58: 时钟滴答间隔 (usec)
    pub ppsfreq: i64,        // 0x60: PPS 频率
    pub jitter: i64,         // 0x68: PPS 抖动
    pub shift: i32,          // 0x70: PPS 间隔稳定性
    _pad2: u32,              // 0x74: 对齐填充
    pub stabil: i64,         // 0x78: PPS 稳定性
    pub jitcnt: i64,         // 0x80: PPS 抖动计数
    pub calcnt: i64,         // 0x88: PPS 校准间隔计数
    pub errcnt: i64,         // 0x90: PPS 校准错误计数
    pub stbcnt: i64,         // 0x98: PPS 稳定性计数
    pub tai: i32,            // 0xa0: TAI 偏移
    _padding: [i32; 11],     // 0xa4: 与 __kernel_timex 尾部对齐
}

impl Timex {
    /// 创建填充了合理默认值的 Timex（对应 modes=0 时的读取操作）
    pub fn defaults() -> Self {
        Self {
            modes: 0,
            _pad0: 0,
            offset: 0,
            freq: 0,
            maxerror: 500000,   // 默认最大误差 500ms
            esterror: 500000,
            status: 0,          // TIME_OK
            _pad1: 0,
            constant: 2,
            precision: 1,
            tolerance: 32768000,
            time: TimeVal::now(),
            tick: 10000,        // 默认 10ms (100Hz)
            ppsfreq: 0,
            jitter: 0,
            shift: 0,
            _pad2: 0,
            stabil: 0,
            jitcnt: 0,
            calcnt: 0,
            errcnt: 0,
            stbcnt: 0,
            tai: 0,
            _padding: [0; 11],
        }
    }
}

unsafe impl Send for Timex {}
unsafe impl Sync for Timex {}

/// CLOCK_REALTIME 的 NTP 调整状态（adjtimex / clock_adjtime 共享）
static REALTIME_TIMEX: Lazy<Mutex<Timex>> = Lazy::new(|| Mutex::new(Timex::defaults()));

const ADJ_OFFSET: u32 = 0x0001;
const ADJ_FREQUENCY: u32 = 0x0002;
const ADJ_MAXERROR: u32 = 0x0004;
const ADJ_ESTERROR: u32 = 0x0008;
const ADJ_STATUS: u32 = 0x0010;
const ADJ_TIMECONST: u32 = 0x0020;
const ADJ_MICRO: u32 = 0x1000;
const ADJ_NANO: u32 = 0x2000;
const ADJ_TICK: u32 = 0x4000;
const ADJ_OFFSET_SS_READ: u32 = 0xa001;
const STA_NANO: i32 = 0x2000;
pub const TIME_OK: usize = 0;
/// Linux USER_HZ（sysconf _SC_CLK_TCK）
const USER_HZ: i64 = 100;

/// 读取当前 REALTIME timex 参数（modes=0 路径）
pub fn timex_get_realtime() -> Timex {
    let mut tx = REALTIME_TIMEX.lock();
    tx.modes = 0;
    tx.time = TimeVal::now();
    *tx
}

/// 应用 timex 调整；`privileged` 为 true 表示具备 CAP_SYS_TIME（简化为 root）
pub fn timex_apply(tx: &Timex, privileged: bool) -> Result<usize, crate::utils::SysErrNo> {
    if tx.modes == 0 {
        return Ok(TIME_OK);
    }

    if !privileged && tx.modes != ADJ_OFFSET_SS_READ {
        return Err(crate::utils::SysErrNo::EPERM);
    }

    if tx.modes == ADJ_OFFSET_SS_READ {
        return Ok(TIME_OK);
    }

    let mut state = REALTIME_TIMEX.lock();
    if tx.modes & ADJ_OFFSET != 0 {
        state.offset = tx.offset;
    }
    if tx.modes & ADJ_FREQUENCY != 0 {
        state.freq = tx.freq;
    }
    if tx.modes & ADJ_MAXERROR != 0 {
        state.maxerror = tx.maxerror;
    }
    if tx.modes & ADJ_ESTERROR != 0 {
        state.esterror = tx.esterror;
    }
    if tx.modes & ADJ_STATUS != 0 {
        state.status = tx.status;
    }
    if tx.modes & ADJ_TIMECONST != 0 {
        state.constant = tx.constant;
    }
    if tx.modes & ADJ_TICK != 0 {
        let min_tick = 900_000 / USER_HZ;
        let max_tick = 1_100_000 / USER_HZ;
        if tx.tick < min_tick || tx.tick > max_tick {
            return Err(crate::utils::SysErrNo::EINVAL);
        }
        state.tick = tx.tick;
    }
    if tx.modes & ADJ_MICRO != 0 {
        state.status &= !STA_NANO;
    }
    if tx.modes & ADJ_NANO != 0 {
        state.status |= STA_NANO;
    }
    state.status = 0;
    Ok(TIME_OK)
}

/// 资源使用统计
#[allow(unused)]
pub struct Rusage {
    pub ru_utime: TimeVal,
    pub ru_stime: TimeVal,
    //unused but needed
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
    pub fn new_from_ms(utime: usize, stime: usize) -> Self {
        let utimeval = TimeVal::new(utime / 1000, (utime % 1000) * 1000);
        let stimeval = TimeVal::new(stime / 1000, (stime % 1000) * 1000);
        Self {
            ru_utime: utimeval,
            ru_stime: stimeval,
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

/// get current time in microseconds
pub fn get_time_ms() -> usize {
    get_ticks() / (get_clock_freq() / MSEC_PER_SEC)
}

/// get current time in nanoseconds
pub fn get_time_ns() -> usize {
    (get_ticks() as u128 * NANOS_PER_SEC as u128 / get_clock_freq() as u128) as usize
}

pub fn wall_time_nanos() -> u64 {
    get_time_ns() as u64 + NOW_TIME_STAMP as u64
}

pub fn wall_time() -> Timespec {
    Timespec::from_nanos(wall_time_nanos())
}

pub fn get_time_spec() -> Timespec {
    let time = get_time_ms();
    Timespec::new(time / 1000, (time % 1000) * 1000000)
}

/// set the next timer interrupt
pub fn set_next_trigger() {
    set_oneshot_timer(get_ticks() + get_clock_freq() / TICKS_PER_SEC);
}

#[derive(Debug, PartialEq, Eq)]
pub enum TimerType {
    Futex,
    StoppedTask,
}

/// 时钟计数器，与 itimer 间隔定时器不同，用于阻塞唤醒进程
pub struct TimerCondVar {
    pub expire: Timespec,
    pub task: Weak<TaskControlBlock>,
    pub kind: TimerType,
    pub extra_data: usize, // 一个计时器服务调用者自行决定用途的额外字段
}
impl PartialEq for TimerCondVar {
    fn eq(&self, other: &Self) -> bool {
        self.expire == other.expire
    }
}
impl Eq for TimerCondVar {}
impl PartialOrd for TimerCondVar {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let a = -(self.expire.to_tick() as isize);
        let b = -(other.expire.to_tick() as isize);
        Some(a.cmp(&b))
    }
}
impl Ord for TimerCondVar {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap()
    }
}

pub static TIMERS: Lazy<Mutex<BinaryHeap<TimerCondVar>>> =
    Lazy::new(|| Mutex::new(BinaryHeap::<TimerCondVar>::new()));

/// 这里的futex_key指的是futex的版本号
/// futex模块用它来区分每一次不同的futex Wait
pub fn add_futex_timer(expire: Timespec, task: &Arc<TaskControlBlock>, futex_key: usize) {
    let mut timers = TIMERS.lock();
    // debug!("add futex timer task {} {}", task.pid(), task.tid());
    timers.push(TimerCondVar {
        expire,
        task: Arc::downgrade(task),
        kind: TimerType::Futex,
        extra_data: futex_key,
    });
}

pub fn add_stopped_task_timer(expire: Timespec, task: Arc<TaskControlBlock>) {
    let mut timers = TIMERS.lock();
    timers.push(TimerCondVar {
        expire,
        task: Arc::downgrade(&task),
        kind: TimerType::StoppedTask,
        extra_data: 0,
    });
}

pub fn check_futex_timer() {
    let mut timers = TIMERS.lock();
    let current = get_time_spec();
    while let Some(timer) = timers.peek() {
        // debug!("expire={:?}, current={:?}", timer.expire, current);
        if timer.expire <= current {
            if let Some(task) = timer.task.upgrade() {
                // debug!("[check_timer] wake up task {} {}", task.pid(), task.tid());
                // debug!("strong count: {}", Arc::strong_count(&task));
                if timer.kind == TimerType::Futex {
                    // 调用 wakeup_task 唤醒超时线程
                    handle_timer(Arc::clone(&task), timer.extra_data);
                } else if timer.kind == TimerType::StoppedTask {
                    todo!()
                    // wakeup_stopped_task(Arc::clone(&task));
                }
            }
            timers.pop();
        } else {
            break;
        }
    }
}
