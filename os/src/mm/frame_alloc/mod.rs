// -------------Public Interfaces------------
// 实际上提供给外界的包括
//  - cma系列函数
//  - FrameTracker类
mod frame_tracker;
mod page_cache;
pub use frame_tracker::FrameTracker;
mod buddy_cma;
pub(crate) use buddy_cma::cancel_cma_lock_owner;
pub use buddy_cma::{cma_alloc, cma_dealloc, init_cma, init_cma_late};
