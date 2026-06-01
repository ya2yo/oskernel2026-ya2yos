///Kernelstack for app
use crate::{arch::memory_layout::PAGE_SIZE, mm::ContinuousPages};

pub struct KernelStackOnHeap {
    pages: ContinuousPages,
}

impl KernelStackOnHeap {
    pub fn new() -> Self {
        Self {
            pages: ContinuousPages::new(4).expect("fail to alloc KStack!"),
        }
    }
    pub fn base(&self) -> usize {
        self.pages.base()
    }

    pub fn top(&self) -> usize {
        self.pages.base() + 4 * PAGE_SIZE
    }
}
