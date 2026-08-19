//! 伪设备文件实现。
//!
//! 本模块把常用的 `/dev/*` 路径映射为内核中的 [`File`] 对象，并提供与
//! Linux 用户态程序约定相符的读、写、轮询、状态查询和少量 `ioctl`
//! 行为。这里的设备主要是内核内建的轻量实现，不对应真实硬件：例如
//! `/dev/zero` 始终产生零字节，`/dev/null` 丢弃写入数据，`/dev/tty`
//! 转发到标准输入输出，而随机设备使用 [`UserBuffer`] 的随机数据填充能力。
//!
//! 设备注册表只负责路径到设备号的关联；真正打开设备时仍由
//! [`open_device_file`] 按路径创建对应对象。loop 设备的路径和控制设备则
//! 交由 [`loopdev`] 模块处理。

use crate::{
    mm::{copy_to_user, MemorySet, UserBuffer},
    syscall::PollEvents,
    timer::realtime,
    utils::{SysErrNo, SyscallRet},
};
use alloc::{
    collections::BTreeMap,
    fmt::{Debug, Formatter},
    format,
    string::{String, ToString},
    sync::Arc,
};
use core::cmp::min;
use core::mem::size_of;
use linux_raw_sys::ioctl::RTC_RD_TIME;
use spin::{Lazy, Mutex, RwLock};

use super::super::{stat::StMode, File, Kstat, Stdin, Stdout};
use super::loopdev;

/// `/dev/zero` 设备：读取时产生任意长度的零字节，写入时直接丢弃数据。
pub struct DevZero;
/// `/dev/null` 设备：读取立即返回 EOF，写入报告全部数据已接受。
pub struct DevNull;
/// 实时时钟设备，支持时间读取以及 `RTC_RD_TIME` 查询。
pub struct DevRtc;
/// `/dev/random` 和 `/dev/urandom` 的共享实现。
///
/// 保存打开时使用的路径，以便 `fstat` 返回与设备节点对应的设备号。
pub struct DevRandom {
    path: String,
}

/// `/dev/tty` 设备，将读写操作转发给内核标准输入输出对象。
pub struct DevTty;

/// `/dev/cpu_dma_latency` 的简化实现。
///
/// 设备内容是一个以大端字节序表示的微秒数，表示进程请求的最大 CPU
/// 反应延迟。读写均通过锁保护，保证不同线程访问同一设备对象时状态一致。
pub struct DevCpuDmaLatency {
    reaction_time: RwLock<u32>, //进程最大反应时间,即CPU最大延迟,单位us
}

/// 设备节点注册表，通过设备路径查找内核分配的设备号。
pub static DEVICES: Lazy<Mutex<BTreeMap<String, usize>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

/// 从 1 开始分配的下一个设备号；0 保留给其他抽象文件。
static mut DEV_NO: usize = 1;

/// Linux 兼容的 `/dev/null` 主、次设备号组合。
const DEV_NULL_RDEV: usize = (1 << 8) | 3;

/// 将设备路径登记到设备号表中。
///
/// 调用方应保证同一路径不会重复注册；重复注册会覆盖旧设备号，且仍会
/// 消耗一个新的递增编号。
pub fn register_device(abs_path: &str) {
    unsafe {
        DEVICES.lock().insert(abs_path.to_string(), DEV_NO);
        DEV_NO += 1;
    }
}

/// 从设备号表中移除指定路径；路径不存在时不产生错误。
pub fn unregister_device(abs_path: &str) {
    DEVICES.lock().remove(&abs_path.to_string());
}

/// 判断路径是否代表已知设备节点。
///
/// loop 设备不逐一写入 [`DEVICES`]，因此控制节点和合法编号的 loop
/// 路径需要额外交给 [`loopdev`] 判断。
pub fn find_device(abs_path: &str) -> bool {
    if abs_path == loopdev::LOOP_CONTROL_PATH || loopdev::parse_loop_device(abs_path).is_some() {
        return true;
    }
    DEVICES.lock().get(abs_path).is_some()
}

/// 获取已登记设备的设备号。
///
/// 该函数假定调用方已经通过 [`find_device`] 确认路径存在；未知路径会
/// 触发 `unwrap`，以便暴露内核内部调用错误。
pub fn get_devno(abs_path: &str) -> usize {
    *DEVICES.lock().get(abs_path).unwrap()
}

/// 根据绝对路径创建内建设备文件对象。
///
/// loop 控制节点及 loop 编号设备优先交由 [`loopdev`] 创建；其余路径
/// 按固定设备名称匹配。未实现或未注册的路径统一返回 `ENOENT`。
pub fn open_device_file(abs_path: &str) -> Result<Arc<dyn File>, SysErrNo> {
    if abs_path == loopdev::LOOP_CONTROL_PATH {
        return Ok(loopdev::DevLoopControl::open());
    }
    if let Some(num) = loopdev::parse_loop_device(abs_path) {
        return Ok(loopdev::DevLoop::open(num, abs_path));
    }
    match abs_path {
        "/dev/zero" => Ok(Arc::new(DevZero::new())),
        "/dev/null" => Ok(Arc::new(DevNull::new())),
        "/dev/rtc" | "/dev/rtc0" | "/dev/misc/rtc" => Ok(Arc::new(DevRtc::new())),
        "/dev/random" | "/dev/urandom" => Ok(Arc::new(DevRandom::new(abs_path))),
        "/dev/tty" => Ok(Arc::new(DevTty::new())),
        "/dev/cpu_dma_latency" => Ok(Arc::new(DevCpuDmaLatency::new())),
        _ => Err(SysErrNo::ENOENT),
    }
}

impl Default for DevZero {
    fn default() -> Self {
        Self::new()
    }
}

/// zero设备
impl DevZero {
    pub fn new() -> Self {
        Self
    }
}

impl File for DevZero {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, mut user_buf: UserBuffer) -> SyscallRet {
        Ok(user_buf.fill0())
    }
    fn write(&self, user_buf: UserBuffer) -> SyscallRet {
        // do nothing
        Ok(user_buf.len())
    }
    fn fstat(&self) -> Kstat {
        let devno = get_devno("/dev/zero");
        Kstat {
            st_dev: devno,
            st_mode: StMode::FCHR.bits(),
            st_rdev: devno,
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
}

impl Default for DevNull {
    fn default() -> Self {
        Self::new()
    }
}

/// NULL设备
impl DevNull {
    pub fn new() -> Self {
        Self
    }
}

impl File for DevNull {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, mut _user_buf: UserBuffer) -> SyscallRet {
        // do nothing
        Ok(0)
    }
    fn write(&self, user_buf: UserBuffer) -> SyscallRet {
        // do nothing
        Ok(user_buf.len())
    }
    fn fstat(&self) -> Kstat {
        let devno = get_devno("/dev/null");
        Kstat {
            st_dev: devno,
            st_mode: StMode::FCHR.bits(),
            st_rdev: DEV_NULL_RDEV,
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
}

/// 便于调试输出的日历时间表示。
///
/// 与 Linux `struct tm` 不同，这个结构直接保存人类可读的年份和月份，
/// 仅用于设备读操作生成文本，不作为用户态 `ioctl` ABI。
pub struct RtcTime {
    pub year: u32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl RtcTime {
    pub fn new(year: u32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }
}

impl Debug for RtcTime {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}-{}-{} {}:{}:{}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

impl Default for DevRtc {
    fn default() -> Self {
        Self::new()
    }
}

/// 时钟设备
impl DevRtc {
    pub fn new() -> Self {
        Self
    }
}

/// 与 Linux `struct rtc_time` 布局兼容的 RTC 时间结构。
///
/// `repr(C)` 保证字段顺序和对齐可直接复制到用户地址空间；月份从 0 开始、
/// 年份保存为相对 1900 的值，符合 Linux RTC 接口约定。
#[repr(C)]
struct LinuxRtcTime {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
}

impl LinuxRtcTime {
    fn from_realtime() -> Self {
        const SECS_PER_DAY: usize = 24 * 60 * 60;

        let seconds = realtime().tv_sec;
        let mut days = seconds / SECS_PER_DAY;
        let seconds_in_day = seconds % SECS_PER_DAY;
        let weekday = ((days + 4) % 7) as i32;
        let mut year = 1970usize;
        while days >= days_in_year(year) {
            days -= days_in_year(year);
            year += 1;
        }

        let yearday = days;
        let mut month = 0usize;
        while days >= days_in_month(year, month) {
            days -= days_in_month(year, month);
            month += 1;
        }

        Self {
            tm_sec: (seconds_in_day % 60) as i32,
            tm_min: ((seconds_in_day / 60) % 60) as i32,
            tm_hour: (seconds_in_day / (60 * 60)) as i32,
            tm_mday: (days + 1) as i32,
            tm_mon: month as i32,
            tm_year: year as i32 - 1900,
            tm_wday: weekday,
            tm_yday: yearday as i32,
            tm_isdst: 0,
        }
    }
}

fn is_leap_year(year: usize) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_year(year: usize) -> usize {
    if is_leap_year(year) {
        366
    } else {
        365
    }
}

fn days_in_month(year: usize, month: usize) -> usize {
    if month == 1 && is_leap_year(year) {
        29
    } else {
        const DAYS_PER_MONTH: [usize; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        DAYS_PER_MONTH[month]
    }
}

impl File for DevRtc {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, mut user_buf: UserBuffer) -> SyscallRet {
        let time = RtcTime::new(2000, 1, 1, 0, 0, 0);
        let str = format!("{:?}", time);
        let bytes = str.as_bytes();
        let len = min(user_buf.len(), bytes.len());
        user_buf.write(bytes);
        Ok(len)
    }
    fn write(&self, user_buf: UserBuffer) -> SyscallRet {
        // do nothing
        Ok(user_buf.len())
    }
    fn fstat(&self) -> Kstat {
        let devno = get_devno("/dev/rtc");
        Kstat {
            st_dev: devno,
            st_mode: StMode::FCHR.bits(),
            st_rdev: devno,
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
    fn ioctl(&self, cmd: u32, arg: usize, memory_set: &MemorySet) -> SyscallRet {
        match cmd {
            RTC_RD_TIME => {
                let rtc_time = LinuxRtcTime::from_realtime();
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(
                        &rtc_time as *const LinuxRtcTime as *const u8,
                        size_of::<LinuxRtcTime>(),
                    )
                })?;
                Ok(0)
            }
            _ => Err(SysErrNo::ENOTTY),
        }
    }
}

impl Default for DevRandom {
    fn default() -> Self {
        Self::new("/dev/random")
    }
}

/// 随机数设备
impl DevRandom {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
        }
    }
}

impl File for DevRandom {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, mut user_buf: UserBuffer) -> SyscallRet {
        Ok(user_buf.fillrandom())
    }
    fn write(&self, user_buf: UserBuffer) -> SyscallRet {
        // do nothing
        Ok(user_buf.len())
    }
    fn fstat(&self) -> Kstat {
        let devno = get_devno(&self.path);
        Kstat {
            st_dev: devno,
            st_mode: StMode::FCHR.bits(),
            st_rdev: devno,
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
}

impl Default for DevTty {
    fn default() -> Self {
        Self::new()
    }
}

/// 终端设备
impl DevTty {
    pub fn new() -> Self {
        Self
    }
}

impl File for DevTty {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, user_buf: UserBuffer) -> SyscallRet {
        Stdin.read(user_buf)
    }
    fn write(&self, user_buf: UserBuffer) -> SyscallRet {
        Stdout.write(user_buf)
    }
    fn fstat(&self) -> Kstat {
        let devno = get_devno("/dev/tty");
        Kstat {
            st_dev: devno,
            st_mode: StMode::FCHR.bits(),
            st_rdev: devno,
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
}

impl Default for DevCpuDmaLatency {
    fn default() -> Self {
        Self::new()
    }
}

/// cpu频率设备
impl DevCpuDmaLatency {
    pub fn new() -> Self {
        Self {
            reaction_time: RwLock::new(10),
        }
    }
}

impl File for DevCpuDmaLatency {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, mut user_buf: UserBuffer) -> SyscallRet {
        let reaction_time = *self.reaction_time.read();
        let buf = [
            (reaction_time >> 24) as u8,
            (reaction_time >> 16) as u8,
            (reaction_time >> 8) as u8,
            reaction_time as u8,
        ];
        Ok(user_buf.write(&buf))
    }
    fn write(&self, user_buf: UserBuffer) -> SyscallRet {
        let mut bytes: [u8; 4] = [0; 4];
        let mut count = 0;
        for sub_buff in user_buf.buffers.iter() {
            let sblen = (*sub_buff).len();
            for j in 0..sblen {
                bytes[count] = (*sub_buff)[j];
                count += 1;
            }
        }
        let mut reaction_time = self.reaction_time.write();
        *reaction_time = (bytes[0] as u32) << 24
            | (bytes[1] as u32) << 16
            | (bytes[2] as u32) << 8
            | bytes[3] as u32;
        Ok(4)
    }
    fn fstat(&self) -> Kstat {
        let devno = get_devno("/dev/cpu_dma_latency");
        Kstat {
            st_dev: devno,
            st_mode: StMode::FCHR.bits(),
            st_rdev: devno,
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
}
