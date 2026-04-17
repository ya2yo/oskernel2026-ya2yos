use core::arch::asm;
use loongArch64::register::{tcfg, ticlr};
use loongArch64::time;

// CPU时钟频率，初始化时被设置
pub static mut CLOCK_FREQ: usize = 0;

/// get current ticks
#[inline(always)]
pub fn get_ticks() -> usize {
    time::Time::read()
}

#[inline(always)]
pub fn get_clock_freq() -> usize {
    unsafe { CLOCK_FREQ }
}

/// 设置一个一次性的定时器
/// 参数ticks是定时器触发的时间间隔
/// Set a one-shot timer.
///
/// A timer interrupt will be triggered at the given ticks
pub fn set_oneshot_timer(ticks: usize) {
    // 清除上一次时钟中断
    ticlr::clear_timer_interrupt();
    let old_ticks = get_ticks();
    let ticks = ticks - old_ticks;
    // align to 4 这是CPU要求
    let ticks = (ticks + 3) & !3;
    tcfg::set_periodic(false); // set timer to one-shot mode
    tcfg::set_init_val(ticks); // set timer initial value
    tcfg::set_en(true); // enable timer
}

pub fn init_clock_freq() {
    unsafe {
        CLOCK_FREQ = time::get_timer_freq();
    }
}
