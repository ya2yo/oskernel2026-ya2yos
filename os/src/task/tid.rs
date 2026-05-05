//!Implementation of [`PidAllocator`]
use core::ops::Deref;

use crate::utils::IdAllocator;
use alloc::vec::Vec;
use spin::{Lazy, Mutex};

// 放弃TrustOS中专用的Tid分配器，分配器逻辑独立出去，成为一个专门的类，见os/src/utils/id_allocator.rs
// 这里仍然使用了懒分配，目的在于使得这个对象被构造得足够晚。由于分配器内部需要维护一个哈希集合，这个对象不能编译时构造
static GLOBAL_ID_ALLOCATOR: Lazy<Mutex<IdAllocator>> = Lazy::new(|| Mutex::new(IdAllocator::new(1)));

///Bind pid lifetime to `TidHandle`
#[derive(Debug,Clone)]
pub struct TidHandle(pub usize);

impl TidHandle {
    /// 尝试分配一个 TID，如果分配失败则返回 None 而不是崩溃
    pub fn alloc() -> Option<Self> {
        GLOBAL_ID_ALLOCATOR.lock().alloc().map(Self)
    }
}

impl Deref for TidHandle {
    type Target = usize;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TidHandle {
    fn drop(&mut self) {
        GLOBAL_ID_ALLOCATOR.lock().dealloc(self.0);
    }
}
