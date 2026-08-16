// ----------------FrameTracker-------------------

use super::page_cache::PAGE_CACHE;
use crate::mm::{PhysAddr, PhysPageNum};
use alloc::sync::Arc;
use core::fmt::{self, Debug, Formatter};

/// Ownership token for one physically contiguous huge-page block.
///
/// A huge mapping keeps one 4 KiB tracker per virtual page for existing VMA
/// bookkeeping, but all trackers share this token so the original aligned CMA
/// allocation is returned exactly once, with its original layout.
pub(crate) struct HugeFrameBlock {
    base_ppn: PhysPageNum,
    pages: usize,
    align_pages: usize,
}

impl HugeFrameBlock {
    pub(crate) fn new(base_ppn: PhysPageNum, pages: usize, align_pages: usize) -> Arc<Self> {
        Arc::new(Self {
            base_ppn,
            pages,
            align_pages,
        })
    }
}

impl Drop for HugeFrameBlock {
    fn drop(&mut self) {
        crate::mm::cma_dealloc_aligned(PhysAddr::from(self.base_ppn), self.pages, self.align_pages);
    }
}

/// manage a frame which has the same lifecycle as the tracker
pub struct FrameTracker {
    pub ppn: PhysPageNum,
    huge_block: Option<Arc<HugeFrameBlock>>,
}

// FrameTracer代表了一个物理页
// 在FrameTracer初始化时，会清空这个物理页
// 在FrameTracer析构时，会释放这个物理页
impl FrameTracker {
    fn new(ppn: PhysPageNum) -> Self {
        let bytes_array = ppn.bytes_array_mut();
        for i in bytes_array {
            *i = 0;
        }
        Self {
            ppn,
            huge_block: None,
        }
    }

    pub fn alloc() -> Option<Arc<FrameTracker>> {
        let ppn = PAGE_CACHE.alloc()?;
        let ret = Some(Arc::new(FrameTracker::new(ppn)));
        ret
    }

    /// Wrap an already selected physical page and clear it before exposure.
    pub(crate) fn from_ppn(ppn: PhysPageNum) -> Arc<FrameTracker> {
        Arc::new(FrameTracker::new(ppn))
    }

    /// Wrap one page from an aligned huge-page allocation.
    pub(crate) fn from_huge_ppn(ppn: PhysPageNum, block: Arc<HugeFrameBlock>) -> Arc<FrameTracker> {
        let mut frame = Self::new(ppn);
        frame.huge_block = Some(block);
        Arc::new(frame)
    }
}

impl Debug for FrameTracker {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("FrameTracker:PPN={:#x}", self.ppn.0))
    }
}

impl Drop for FrameTracker {
    fn drop(&mut self) {
        if self.huge_block.is_none() {
            PAGE_CACHE.dealloc(self.ppn);
        }
    }
}
