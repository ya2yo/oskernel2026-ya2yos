mod error;
mod id_allocator;
pub mod perf;
mod resource_slot;
pub mod simple_range;
mod string;
use core::arch::asm;
pub mod poll;
pub use poll::*;
// pub use command::*;
use crate::arch::memory_layout::PAGE_SIZE;
use crate::mm::VirtAddr;
pub use error::*;
pub use id_allocator::*;
use log::warn;
pub use resource_slot::*;
pub use string::*;

/// 跟踪函数的调用栈
pub fn backtrace() {
    // 算了，我们不应该指望这玩意能用
    // unsafe {
    //     let mut fp: usize;
    //     asm!("mv {}, fp", out(reg) fp);
    //     let mut start: VirtAddr = VirtAddr::from(fp).floor().into();
    //     let mut end: VirtAddr = VirtAddr::from(fp).ceil().into();
    //     let mut fp_addr = VirtAddr::from(fp);
    //     while start <= fp_addr && fp_addr < end {
    //         let ptr = fp as *const usize;
    //         warn!("[stack_backtrace] {:#x},", ptr.offset(-8).read());
    //         fp = ptr.offset(-16).read();
    //         start = VirtAddr::from(fp).floor().into();
    //         end = VirtAddr::from(fp).ceil().into();
    //         fp_addr = VirtAddr::from(fp);
    //     }
    // }
}
/// 对齐到页
pub fn page_round_up(v: usize) -> usize {
    if v.is_multiple_of(PAGE_SIZE) {
        v
    } else {
        v - (v % PAGE_SIZE) + PAGE_SIZE
    }
}
