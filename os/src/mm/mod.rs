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
use core::{arch::asm, fmt::Debug};

use crate::utils::simple_range::{SimpleRange, StepByOne};

/// a simple range structure for virtual page number
type VPNRange = SimpleRange<VirtPageNum>;

pub use address::{KernelAddr, PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
pub use frame_alloc::{cma_alloc, cma_dealloc, FrameTracker};
pub use map_area::{MapArea, MapAreaType, MapPermission};
pub(crate) use memory_set::{read_elf_load_image, read_elf_load_image_with_prefix};
pub use memory_set::{MemorySet, MemorySetInner, KERNEL_SPACE};
pub use mmap_bad_address::{if_bad_address, insert_bad_address, remove_bad_address};

// pub use page_table::{PTEFlags, PageTable, PageTableEntry};

pub mod translate;
pub use translate::*;

pub use heap_allocator::ContinuousPages;
pub use shm::{shm_attach, shm_create, shm_detach, shm_drop, shm_find, ShmFlags};

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
    frame_alloc::init_cma_late();
    println!("mm:cma late range inited");
    heap_allocator::enable_cma_backing();
    println!("mm:heap CMA backing enabled");
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

impl VPNRange {
    pub fn contains_vpn(self, other: VirtPageNum) -> bool {
        self.start() <= other && other < self.end()
    }
}

impl Debug for VPNRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VPNRange")
            .field("start", &self.start())
            .field("end", &self.end())
            .finish()
    }
}
