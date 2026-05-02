use core::{alloc::Layout, ptr::NonNull};

use buddy_system_allocator::LockedHeap;

use crate::{
    arch::memory_layout::{
        KERNEL_ADDR_OFFSET, PAGE_SIZE, PHYSICAL_MEMORY_SIZE, PHYSICAL_MEMORY_START,
    },
    mm::{KernelAddr, PhysAddr},
};

// ----------------CMA-------------------
// 基于伙伴算法的连续物理地址分配器
// 除了内核本身的各ELF段（堆在数据段里面）之外
// 其他所有的空闲空间均用伙伴算法管理
static CMA_ALLOCATOR: LockedHeap = LockedHeap::empty();

/// initiate heap allocator
pub fn init_cma() {
    extern "C" {
        fn ekernel();
    }
    assert!(ekernel as *const() as usize % 4096 == 0);
    assert!(PHYSICAL_MEMORY_START % 4096 == 0);
    assert!(PHYSICAL_MEMORY_SIZE % 4096 == 0);

    // kernel使用的空间大小+kernel之前为MMIO保留的空间大小
    let used_physical_memory = (ekernel as *const() as usize - PHYSICAL_MEMORY_START) - KERNEL_ADDR_OFFSET;
    let size = PHYSICAL_MEMORY_SIZE - used_physical_memory;
    let left = KERNEL_ADDR_OFFSET + PHYSICAL_MEMORY_START + used_physical_memory;
    println!("init_cma:");
    println!("from: {:#x}", left);
    println!("size: {:#x}", size);
    println!("to:   {:#x}", left + size);
    assert!(size > 0);

    unsafe {
        CMA_ALLOCATOR.lock().init(
            left, // 其实就是ekernel???
            size,
        );
    }

    // println!(
    //     "CMA allocator initialized: start = {:#x}, size = {:#x}",
    //     KERNEL_ADDR_OFFSET + PHYSICAL_MEMORY_START + used_physical_memory,
    //     size
    // );
    return;
}

/// 分配连续物理内存页（返回起始物理地址）
pub fn cma_alloc(pages: usize) -> Option<PhysAddr> {
    let layout_opt = Layout::from_size_align(
        pages * PAGE_SIZE,
        PAGE_SIZE, // 按页对齐
    )
    .ok();
    match layout_opt {
        Some(layout) => {
            let mut locked = CMA_ALLOCATOR.lock();
            let ptr_result = locked.alloc(layout);
            let ptr = ptr_result.unwrap().as_ptr() as usize;
            assert_eq!(ptr % PAGE_SIZE, 0);
            let va = KernelAddr::from(ptr);
            let pa = PhysAddr::from(va);
            Some(pa)
        }
        None => None,
    }
}

/// 释放连续物理内存
pub fn cma_dealloc(paddr: PhysAddr, pages: usize) {
    assert_eq!(paddr.0 % PAGE_SIZE, 0);
    let layout =
        Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).expect("Invalid deallocation layout");
    let va = KernelAddr::from(paddr);
    let ptr = NonNull::new(va.0 as *mut u8).expect("Pointer must not be null!");
    CMA_ALLOCATOR.lock().dealloc(ptr, layout);
}
