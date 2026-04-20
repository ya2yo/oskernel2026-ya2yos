//! LoongArch64 架构相关的硬件时间实现
use loongArch64::register::{tcfg, ticlr};
use loongArch64::time;

// CPU时钟频率，初始化时从硬件获取
pub static mut CLOCK_FREQ: usize = 0;

/// 统一的硬件抽象接口
pub trait TimeInterface {
    fn current_ticks() -> u64;
    fn ticks_to_nanos(ticks: u64) -> u64;
    fn nanos_to_ticks(nanos: u64) -> u64;
    fn set_oneshot_timer(deadline_ns: u64); // 注意：这里传入的是绝对时间（纳秒）
}

pub struct TimeImpl;

impl TimeInterface for TimeImpl {
    #[inline(always)]
    fn current_ticks() -> u64 {
        time::Time::read() as u64
    }

    #[inline]
    fn ticks_to_nanos(ticks: u64) -> u64 {
        let freq = unsafe { CLOCK_FREQ };
        if freq == 0 { return 0; }
        (ticks as u128 * 1_000_000_000 / freq as u128) as u64
    }

    #[inline]
    fn nanos_to_ticks(nanos: u64) -> u64 {
        let freq = unsafe { CLOCK_FREQ };
        (nanos as u128 * freq as u128 / 1_000_000_000) as u64
    }

    /// 龙芯的实现逻辑：
    /// 1. 将绝对纳秒死线转换为绝对 Ticks
    /// 2. 计算相对差值 (deadline - current)
    /// 3. 设置 TCFG 寄存器
    fn set_oneshot_timer(deadline_ns: u64) {
        // 1. 清除上一次中断标志
        ticlr::clear_timer_interrupt();

        // 2. 转换绝对死线为绝对 Ticks
        let deadline_ticks = Self::nanos_to_ticks(deadline_ns);
        let now_ticks = Self::current_ticks();

        // 3. 计算相对触发间隔
        let ticks = if deadline_ticks > now_ticks {
            deadline_ticks - now_ticks
        } else {
            // 如果死线已经过了，设置一个极小值立即触发
            4 
        };

        // 4. 对齐要求：低 2 位清零（4 字节对齐），这是龙芯 TCFG 的硬件要求
        let aligned_ticks = (ticks + 3) & !3;

        // 5. 写入硬件寄存器
        tcfg::set_periodic(false);      // 单次触发模式
        tcfg::set_init_val(aligned_ticks as usize); // 设置倒计时初始值
        tcfg::set_en(true);             // 使能定时器
    }
}

/// 初始化频率（通常在启动阶段由设备树或 CPUID 获取）
pub fn init_clock_freq() {
    unsafe {
        CLOCK_FREQ = time::get_timer_freq();
    }
}

#[inline(always)]
pub fn get_clock_freq() -> usize {
    unsafe { CLOCK_FREQ }
}