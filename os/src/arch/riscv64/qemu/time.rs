use core::sync::atomic::{AtomicUsize, Ordering};
use riscv::register::time;
use sbi_rt;

static CLOCK_FREQ: AtomicUsize = AtomicUsize::new(10_000_000);

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
    CLOCK_FREQ.load(Ordering::Acquire)
}

pub fn init_clock_freq() {
    CLOCK_FREQ.store(crate::arch::hardware::timebase_hz(), Ordering::Release);
}
