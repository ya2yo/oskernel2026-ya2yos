use riscv::register::time;
use sbi_rt;

pub const CLOCK_FREQ: usize = 0x989680; // 由设备树文件获取

/// get current ticks
pub fn get_ticks() -> usize {
    time::read()
}

/// 设置一个一次性的定时器
/// 参数ticks是定时器触发的时间，是一个绝对时间而不是一个间隔
/// Set a one-shot timer.
///
/// A timer interrupt will be triggered at the given ticks
pub fn set_oneshot_timer(ticks: usize) {
    sbi_rt::set_timer(ticks as u64);
}

#[inline(always)]
pub fn get_clock_freq() -> usize {
    CLOCK_FREQ
}

pub fn init_clock_freq() {}
