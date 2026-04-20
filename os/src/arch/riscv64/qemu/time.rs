//! RISC-V 架构相关的硬件时间实现
use riscv::register::time;
use sbi_rt;

/// 由设备树获取的硬件时钟频率
pub const CLOCK_FREQ: u64 = 0x989680; 

/// 硬件层必须实现的接口
pub trait TimeInterface {
    fn current_ticks() -> u64;
    fn ticks_to_nanos(ticks: u64) -> u64;
    fn nanos_to_ticks(nanos: u64) -> u64;
    fn set_oneshot_timer(deadline_ns: u64);
}

pub struct TimeImpl;

impl TimeInterface for TimeImpl {
    #[inline]
    fn current_ticks() -> u64 {
        time::read() as u64
    }

    #[inline]
    fn ticks_to_nanos(ticks: u64) -> u64 {
        (ticks as u128 * 1_000_000_000 / CLOCK_FREQ as u128) as u64
    }

    #[inline]
    fn nanos_to_ticks(nanos: u64) -> u64 {
        (nanos as u128 * CLOCK_FREQ as u128 / 1_000_000_000) as u64
    }

    fn set_oneshot_timer(deadline_ns: u64) {
        let ticks = Self::nanos_to_ticks(deadline_ns);
        sbi_rt::set_timer(ticks);
    }
}