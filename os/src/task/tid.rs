//!Implementation of [`PidAllocator`]
use crate::utils::IdAllocator;
use alloc::vec::Vec;
use spin::{Lazy, Mutex};

// 放弃TrustOS中专用的Tid分配器，分配器逻辑独立出去，成为一个专门的类，见os/src/utils/id_allocator.rs
// 这里仍然使用了懒分配，目的在于使得这个对象被构造得足够晚。由于分配器内部需要维护一个哈希集合，这个对象不能编译时构造
static TID_ALLOCATOR: Lazy<Mutex<IdAllocator>> = Lazy::new(|| Mutex::new(IdAllocator::new()));

///Bind pid lifetime to `TidHandle`

pub struct TidHandle {
    pub tid: usize,
}

impl TidHandle {
    pub fn new() -> Self {
        TidHandle {
            tid: TID_ALLOCATOR.lock().alloc().unwrap(),
        }
    }
}

impl Drop for TidHandle {
    fn drop(&mut self) {
        TID_ALLOCATOR.lock().dealloc(self.tid);
    }
}
