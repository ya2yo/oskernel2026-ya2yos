use core::{alloc::Layout, ptr::NonNull};

use buddy_system_allocator::LockedHeap;

#[cfg(target_arch = "loongarch64")]
use crate::arch::memory_layout::PHYSICAL_MEMORY_RANGES;
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
    assert!(ekernel as *const () as usize % 4096 == 0);
    assert!(PHYSICAL_MEMORY_START % 4096 == 0);
    assert!(PHYSICAL_MEMORY_SIZE % 4096 == 0);

    println!("init_cma:");
    let ekernel_va = ekernel as *const () as usize;
    let total = init_cma_heap(ekernel_va);
    assert!(total > 0);

    // println!(
    //     "CMA allocator initialized: start = {:#x}, size = {:#x}",
    //     KERNEL_ADDR_OFFSET + PHYSICAL_MEMORY_START,
    //     total
    // );
    return;
}

#[cfg(target_arch = "loongarch64")]
fn init_cma_heap(ekernel_va: usize) -> usize {
    let mut total = 0usize;
    let mut allocator = CMA_ALLOCATOR.lock();
    for &(start, size) in PHYSICAL_MEMORY_RANGES {
        assert!(start % PAGE_SIZE == 0);
        assert!(size % PAGE_SIZE == 0);
        let range_start = KERNEL_ADDR_OFFSET + start;
        let range_end = range_start + size;
        let left = if ekernel_va > range_start && ekernel_va < range_end {
            ekernel_va
        } else {
            range_start
        };
        if left >= range_end {
            continue;
        }
        println!("from: {:#x}", left);
        println!("size: {:#x}", range_end - left);
        println!("to:   {:#x}", range_end);
        unsafe {
            allocator.add_to_heap(left, range_end);
        }
        total += range_end - left;
    }
    total
}

#[cfg(not(target_arch = "loongarch64"))]
fn init_cma_heap(ekernel_va: usize) -> usize {
    // kernel使用的空间大小+kernel之前为MMIO保留的空间大小
    let used_physical_memory = (ekernel_va - PHYSICAL_MEMORY_START) - KERNEL_ADDR_OFFSET;
    let size = PHYSICAL_MEMORY_SIZE - used_physical_memory;
    let left = KERNEL_ADDR_OFFSET + PHYSICAL_MEMORY_START + used_physical_memory;
    println!("from: {:#x}", left);
    println!("size: {:#x}", size);
    println!("to:   {:#x}", left + size);
    unsafe {
        CMA_ALLOCATOR.lock().init(
            left, // 其实就是ekernel???
            size,
        );
    }
    size
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
            let ptr_result = match locked.alloc(layout) {
                Ok(ptr) => ptr,
                Err(_) => return None,
            };
            let ptr = ptr_result.as_ptr() as usize;
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
