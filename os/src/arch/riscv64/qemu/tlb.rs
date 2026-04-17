use core::arch::asm;

/// 清空所有tlb条目
#[inline(always)]
pub fn tlb_invalidate() {
    // TODO:有没有更精确的实现，全部清空是否代价过高
    unsafe {
        asm!("sfence.vma");
    }
}
