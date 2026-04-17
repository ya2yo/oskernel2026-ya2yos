use core::arch::asm;

/// 清空所有tlb条目
#[inline(always)]
pub fn tlb_invalidate() {
    unsafe {
        asm!("invtlb 0x0,$zero, $zero");
    }
}
