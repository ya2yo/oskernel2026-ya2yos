use core::arch::asm;

/// 清空所有tlb条目
#[inline(always)]
pub fn tlb_invalidate() {
    // TODO:有没有更精确的实现，全部清空是否代价过高
    unsafe {
        asm!("sfence.vma");
    }
}

/// Make stores to newly populated executable pages visible to instruction
/// fetches on the current hart.
#[inline(always)]
pub fn instruction_fence() {
    unsafe {
        asm!("fence.i");
    }
}
