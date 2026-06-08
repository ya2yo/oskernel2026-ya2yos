//! Linux `struct timex` — NTP 时钟同步参数 (adjtimex / clock_adjtime)
//!
//! # 用途
//! - **`adjtimex(2)`**: 读取/设置系统时钟同步参数 (NTP daemon 使用)
//! - **`clock_adjtime(2)`**: 同上，但指定 clock_id (仅 CLOCK_REALTIME 支持)
//!
//! # 核心操作
//! - `modes == 0`: 读取当前 timex 参数 (`timex_get_realtime()`)
//! - `modes != 0`: 根据 `modes` 位掩码应用对应字段 (`timex_apply()`)
//!
//! # 结构体布局
//! `#[repr(C)]` 与 Linux RISC-V 64-bit `struct timex` 二进制兼容。
//! 关键字段偏移: modes(0x00), offset(0x08), freq(0x10), status(0x28), tick(0x58)
//!
//! # NTP 模式常量 (ADJ_*)
//! 与 Linux `<sys/timex.h>` 一致:
//! - ADJ_OFFSET (0x0001): 设置时间偏移
//! - ADJ_FREQUENCY (0x0002): 设置频率偏移
//! - ADJ_MAXERROR (0x0004): 设置最大误差
//! - ADJ_ESTERROR (0x0008): 设置估计误差
//! - ADJ_STATUS (0x0010): 设置时钟状态
//! - ADJ_TIMECONST (0x0020): 设置 PLL 时间常数
//! - ADJ_TICK (0x4000): 设置时钟滴答间隔
//! - ADJ_OFFSET_SS_READ (0xa001): 读取 offset (非特权)

use spin::{Lazy, Mutex};

use super::timeval::TimeVal;

/// adjtimex 成功时返回 TIME_OK
pub const TIME_OK: usize = 0;

/// Linux USER_HZ (sysconf _SC_CLK_TCK = 100)
const USER_HZ: i64 = 100;

// ---- NTP 调整模式位掩码 ----
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

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Timex {
    pub modes: u32,
    _pad0: u32,
    pub offset: i64,
    pub freq: i64,
    pub maxerror: i64,
    pub esterror: i64,
    pub status: i32,
    _pad1: u32,
    pub constant: i64,
    pub precision: i64,
    pub tolerance: i64,
    pub time: TimeVal,
    pub tick: i64,
    pub ppsfreq: i64,
    pub jitter: i64,
    pub shift: i32,
    _pad2: u32,
    pub stabil: i64,
    pub jitcnt: i64,
    pub calcnt: i64,
    pub errcnt: i64,
    pub stbcnt: i64,
    pub tai: i32,
    _padding: [i32; 11],
}

impl Timex {
    /// 创建填充了合理默认值的 Timex (modes=0 读取时返回)
    pub fn defaults() -> Self {
        Self {
            modes: 0,
            _pad0: 0,
            offset: 0,
            freq: 0,
            maxerror: 500_000,
            esterror: 500_000,
            status: 0,
            _pad1: 0,
            constant: 2,
            precision: 1,
            tolerance: 32_768_000,
            time: TimeVal::now(),
            tick: 10_000, // 10ms (100Hz)
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

/// CLOCK_REALTIME 的 NTP 持久状态 (adjtimex / clock_adjtime 共享)
static REALTIME_TIMEX: Lazy<Mutex<Timex>> = Lazy::new(|| Mutex::new(Timex::defaults()));

/// 读取当前 REALTIME timex 参数 (modes=0 路径)
pub fn timex_get_realtime() -> Timex {
    let mut tx = REALTIME_TIMEX.lock();
    tx.modes = 0;
    tx.time = TimeVal::now();
    *tx
}

/// 应用 timex 调整
///
/// `privileged` 为 true 表示具备 CAP_SYS_TIME (简化为 root)。
/// 非特权进程仅允许 `ADJ_OFFSET_SS_READ` (只读偏移量)。
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

    state.status = 0; // 标记为 TIME_OK
    Ok(TIME_OK)
}
