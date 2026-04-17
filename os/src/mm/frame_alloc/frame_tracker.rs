// ----------------FrameTracker-------------------

use super::page_cache::PAGE_CACHE;
use crate::mm::PhysPageNum;
use alloc::sync::Arc;
use core::fmt::{self, Debug, Formatter};
use log::debug;

/// manage a frame which has the same lifecycle as the tracker

pub struct FrameTracker {
    pub ppn: PhysPageNum,
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
        Self { ppn }
    }

    pub fn alloc() -> Option<Arc<FrameTracker>> {
        let ppn = PAGE_CACHE.alloc()?;
        let ret = Some(Arc::new(FrameTracker::new(ppn)));
        ret
    }
}

impl Debug for FrameTracker {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("FrameTracker:PPN={:#x}", self.ppn.0))
    }
}

impl Drop for FrameTracker {
    fn drop(&mut self) {
        PAGE_CACHE.dealloc(self.ppn);
    }
}
