//! Memory management implementation
//!
//! SV39 page-based virtual-memory architecture for RV64 systems, and
//! everything about memory management, like frame allocator, page table,
//! map area and memory set, is implemented here.
//!
//! Every task or process has a memory_set to control its virtual memory.
pub mod address;
mod frame_alloc;
mod group;
mod heap_allocator;
mod map_area;
mod memory_set;
mod mmap_bad_address;
mod page_fault_handler;
// mod page_table;
mod shm;
use core::arch::asm;

use crate::utils::simple_range::{SimpleRange, StepByOne};

/// a simple range structure for virtual page number
type VPNRange = SimpleRange<VirtPageNum>;

pub use address::{KernelAddr, PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
pub use frame_alloc::{cma_alloc, cma_dealloc, FrameTracker};
pub use map_area::{MapArea, MapAreaType, MapPermission};
pub use memory_set::{MemorySet, MemorySetInner, KERNEL_SPACE};
pub use mmap_bad_address::{if_bad_address, insert_bad_address, remove_bad_address};

// pub use page_table::{PTEFlags, PageTable, PageTableEntry};

pub mod translate;
pub use translate::{
    get_data, put_data, safe_get_data, safe_put_data, safe_translated_byte_buffer,
    translated_byte_buffer, translated_ref, translated_refmut, translated_str, UserBuffer,
    UserBufferIterator,
};

pub use heap_allocator::ContinuousPages;
pub use shm::{shm_attach, shm_create, shm_drop, shm_find, ShmFlags};

// 这一步会执行切换页表操作
pub fn activate_kernel_space() {
    KERNEL_SPACE.lock().activate();
}

/// initiate heap allocator, frame allocator and kernel space
pub fn init() {
    println!("initing mm");
    heap_allocator::init_heap();
    println!("mm:heap inited");
    frame_alloc::init_cma();
    println!("mm:cma inited");
    activate_kernel_space();
    println!("mm:kernel pagetable activated");
    memory_set::remap_test();
    println!("mm:remap_test complete, mm_init is finished");
}

bitflags! {

    pub struct MremapFlags: i32 {

        const MAYMOVE    = 1 << 0;

        const FIXED      = 1 << 1;

        const DONTUNMAP  = 1 << 2;
    }
}
