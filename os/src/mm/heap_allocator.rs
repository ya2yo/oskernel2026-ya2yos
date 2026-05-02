//! The global allocator
use core::{alloc::Layout, cell::{SyncUnsafeCell, UnsafeCell}, ptr::NonNull};

use crate::arch::memory_layout::{KERNEL_HEAP_SIZE, PAGE_SIZE};
use buddy_system_allocator::LockedHeap;

#[global_allocator]
/// heap allocator instance
static HEAP_ALLOCATOR: LockedHeap = LockedHeap::empty();

#[alloc_error_handler]
/// panic when heap allocation error occurs
pub fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    panic!("Heap allocation error, layout = {:?}", layout);
}

#[repr(align(4096))] // 强制 4K 对齐
struct HeapSpace([u8; KERNEL_HEAP_SIZE]);

// SyncUnsafeCell虽然线程不安全，但是有分配器的锁进行保护，只是需要内部可变性
static HEAP_SPACE: SyncUnsafeCell<HeapSpace>  = SyncUnsafeCell::new(
    HeapSpace([0; KERNEL_HEAP_SIZE])
);

/// initiate heap allocator
pub fn init_heap() {
    unsafe {
        HEAP_ALLOCATOR
            .lock()
            .init(HEAP_SPACE.get() as usize, KERNEL_HEAP_SIZE);
    }
}

#[allow(unused)]
pub fn heap_test() {
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    extern "C" {
        fn sbss();
        fn ebss();
    }
    let bss_range = sbss as *const() as usize..ebss as *const() as usize;
    let a = Box::new(5);
    assert_eq!(*a, 5);
    assert!(bss_range.contains(&(a.as_ref() as *const _ as usize)));
    drop(a);
    let mut v: Vec<usize> = Vec::new();
    for i in 0..500 {
        v.push(i);
    }
    for (i, val) in v.iter().take(500).enumerate() {
        assert_eq!(*val, i);
    }
    assert!(bss_range.contains(&(v.as_ptr() as usize)));
    drop(v);
    println!("heap_test passed!");
}

pub struct ContinuousPages {
    base: usize,
    page_num: usize,
    layout: Layout,
}

unsafe impl Send for ContinuousPages {}
unsafe impl Sync for ContinuousPages {}

impl ContinuousPages {
    pub fn new(page_num: usize) -> Option<Self> {
        let size = page_num * PAGE_SIZE;

        // 创建页面对齐的内存布局
        let layout = match Layout::from_size_align(size, PAGE_SIZE) {
            Ok(layout) => layout,
            Err(_) => return None, // 布局无效
        };

        // 通过全局分配器分配内存
        let ptr = HEAP_ALLOCATOR.lock().alloc(layout);

        match ptr {
            Err(_) => return None,
            Ok(ptr) => {
                let ptr_val = ptr.as_ptr() as usize;
                // 验证对齐是否符合要求（可选但推荐）
                assert_eq!(ptr_val % PAGE_SIZE, 0, "Allocated memory not page aligned!");
                return Some(Self {
                    base: ptr_val,
                    page_num: size,
                    layout,
                });
            }
        }
    }

    pub fn base(&self) -> usize {
        return self.base;
    }
}

impl Drop for ContinuousPages {
    fn drop(&mut self) {
        let non_null = NonNull::new(self.base as *mut u8).unwrap();
        HEAP_ALLOCATOR.lock().dealloc(non_null, self.layout);
    }
}
