use core::arch::asm;

/// 清空所有tlb条目
#[inline(always)]
pub fn tlb_invalidate() {
    unsafe {
        asm!("invtlb 0x0,$zero, $zero");
    }
}

/// Make newly populated executable pages visible to instruction fetches on
/// the current hart.
#[inline(always)]
pub fn instruction_fence() {
    unsafe {
        asm!("ibar 0");
    }
}
