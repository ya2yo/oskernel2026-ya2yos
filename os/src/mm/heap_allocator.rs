//! The global allocator
use core::{
    alloc::{GlobalAlloc, Layout},
    cell::{SyncUnsafeCell, UnsafeCell},
    cmp::max,
    ptr::{self, NonNull},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::arch::memory_layout::{KERNEL_HEAP_SIZE, PAGE_SIZE};
use buddy_system_allocator::LockedHeap;
use spin::Mutex;

#[global_allocator]
/// heap allocator instance
static HEAP_ALLOCATOR: KernelHeapAllocator = KernelHeapAllocator;

/// The buddy heap that backs Rust's global allocator.
///
/// It starts with the low-memory BSS range and can receive additional,
/// CMA-owned ranges after MM initialization has made every architecture's
/// direct map usable.
static HEAP: LockedHeap = LockedHeap::empty();

/// Do not touch CMA before `mm::init()` has completed its allocator setup.
static CMA_HEAP_BACKING_READY: AtomicBool = AtomicBool::new(false);

/// Serializes a failed allocation's retry through CMA.  The global heap lock
/// is deliberately released before taking this lock and before using CMA.
static CMA_HEAP_GROW_LOCK: Mutex<()> = Mutex::new(());

/// Bytes permanently transferred from CMA to the global kernel heap.
static CMA_HEAP_BACKING_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Cap intrusive buddy-list work while holding the global allocator mutex.
/// BuildStorm creates enough short-lived metadata objects that a small free
/// can otherwise scan an unbounded list and stall every kernel allocation.
/// A skipped merge leaves the block immediately reusable in its current class;
/// CMA-backed heap growth remains available for genuinely larger requests.
const HEAP_BUDDY_SCAN_LIMIT: usize = 256;

/// Prefer a substantial high-memory range when recovering from heap pressure.
/// On the LoongArch QEMU layout the remaining low RAM is smaller than this,
/// so the first extension naturally comes from the high RAM segment.
const CMA_HEAP_GROW_MIN: usize = 0x0800_0000;

struct KernelHeapAllocator;

unsafe impl GlobalAlloc for KernelHeapAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        alloc_from_heap(layout)
            .ok()
            .map_or(ptr::null_mut(), |ptr| ptr.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HEAP.lock().dealloc_with_bounded_merge(
            NonNull::new_unchecked(ptr),
            layout,
            HEAP_BUDDY_SCAN_LIMIT,
        );
    }
}

/// Return the buddy block size required by `layout`, rounded to a page so it
/// can be handed over from CMA without overlap or an ownership gap.
fn required_cma_bytes(layout: Layout) -> Option<usize> {
    let requested = layout.size().checked_next_power_of_two()?;
    Some(max(max(requested, layout.align()), PAGE_SIZE))
}

/// Allocate from the global heap, expanding it from CMA once when necessary.
/// Keeping this outside `GlobalAlloc` also lets page-aligned kernel buffers use
/// the same fallback rather than silently bypassing it.
fn alloc_from_heap(layout: Layout) -> Result<NonNull<u8>, ()> {
    let first_attempt = { HEAP.lock().alloc(layout) };
    match first_attempt {
        Ok(ptr) => Ok(ptr),
        Err(_) => grow_from_cma(layout),
    }
}

/// Expand the global heap with a range first allocated from CMA, then reserve
/// the original request before another hart can consume that new range.
///
/// CMA owns the pages until `add_to_heap()` succeeds.  Once transferred, the
/// range is never passed to `cma_dealloc()` because the global buddy heap owns
/// all subsequent suballocations and frees within that range.
fn grow_from_cma(layout: Layout) -> Result<NonNull<u8>, ()> {
    if !CMA_HEAP_BACKING_READY.load(Ordering::Acquire) {
        return Err(());
    }

    let required = match required_cma_bytes(layout) {
        Some(bytes) => bytes,
        None => return Err(()),
    };
    let _grow_guard = CMA_HEAP_GROW_LOCK.lock();

    // Another hart may have grown the heap after the first failed attempt.
    // Reserve the request itself here instead of probing and releasing it, so
    // the retry cannot turn into a false OOM under SMP contention.
    if let Ok(ptr) = { HEAP.lock().alloc(layout) } {
        return Ok(ptr);
    }

    let mut grow_size = max(required, CMA_HEAP_GROW_MIN);
    loop {
        let pages = grow_size / PAGE_SIZE;
        if let Some(paddr) = crate::mm::cma_alloc(pages) {
            let start = crate::mm::KernelAddr::from(paddr).0;
            let Some(end) = start.checked_add(grow_size) else {
                crate::mm::cma_dealloc(paddr, pages);
                return Err(());
            };
            unsafe {
                let mut heap = HEAP.lock();
                heap.add_to_heap(start, end);
                CMA_HEAP_BACKING_BYTES.fetch_add(grow_size, Ordering::Relaxed);
                return heap.alloc(layout);
            }
        }

        // A fragmented CMA may not have the preferred 128 MiB block even
        // though it can still satisfy the failed heap allocation.  Retry down
        // to exactly the required buddy block before declaring OOM.
        if grow_size == required {
            return Err(());
        }
        grow_size = max(required, grow_size / 2);
    }
}

#[alloc_error_handler]
/// panic when heap allocation error occurs
pub fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    let (user, actual, total) = {
        let heap = HEAP.lock();
        (
            heap.stats_alloc_user(),
            heap.stats_alloc_actual(),
            heap.stats_total_bytes(),
        )
    };
    panic!(
        "Heap allocation error, layout = {:?}, user = {:#x}, actual = {:#x}, total = {:#x}, cma_backing = {:#x}",
        layout,
        user,
        actual,
        total,
        CMA_HEAP_BACKING_BYTES.load(Ordering::Relaxed),
    );
}

#[repr(align(4096))] // 强制 4K 对齐
struct HeapSpace([u8; KERNEL_HEAP_SIZE]);

// SyncUnsafeCell虽然线程不安全，但是有分配器的锁进行保护，只是需要内部可变性
static HEAP_SPACE: SyncUnsafeCell<HeapSpace> =
    SyncUnsafeCell::new(HeapSpace([0; KERNEL_HEAP_SIZE]));

/// initiate heap allocator
pub fn init_heap() {
    unsafe {
        HEAP.lock()
            .init(HEAP_SPACE.get() as usize, KERNEL_HEAP_SIZE);
    }
}

/// Allow failed global-heap allocations to borrow real physical memory from
/// CMA.  This must run after all architecture-specific CMA ranges are mapped.
pub fn enable_cma_backing() {
    CMA_HEAP_BACKING_READY.store(true, Ordering::Release);
}

#[allow(unused)]
pub fn heap_test() {
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    extern "C" {
        fn sbss();
        fn ebss();
    }
    let bss_range = sbss as *const () as usize..ebss as *const () as usize;
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
        let size = page_num.checked_mul(PAGE_SIZE)?;

        // 创建页面对齐的内存布局
        let layout = match Layout::from_size_align(size, PAGE_SIZE) {
            Ok(layout) => layout,
            Err(_) => return None, // 布局无效
        };

        // 通过全局分配器分配内存
        let ptr = alloc_from_heap(layout);

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
        HEAP.lock()
            .dealloc_with_bounded_merge(non_null, self.layout, HEAP_BUDDY_SCAN_LIMIT);
    }
}
