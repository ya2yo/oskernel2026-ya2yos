use core::{
    alloc::Layout,
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use buddy_system_allocator::Heap;

#[cfg(target_arch = "riscv64")]
use crate::arch::memory_layout::BOOTSTRAP_PHYSICAL_MEMORY_SIZE;
use crate::{
    arch::memory_layout::{KERNEL_ADDR_OFFSET, PAGE_SIZE},
    mm::{KernelAddr, PhysAddr},
};

// ----------------CMA-------------------
// 基于伙伴算法的连续物理地址分配器
// 除了内核本身的各ELF段（堆在数据段里面）之外
// 其他所有的空闲空间均用伙伴算法管理
//
// Do not use `LockedHeap` here.  Its dependency on spin 0.7 is a ticket
// lock: a task that is removed by the scheduler after taking a ticket can
// permanently strand every later allocator caller.  Task exit does not unwind
// the abandoned kernel stack, so the usual RAII unlock is not sufficient.
const CMA_UNLOCKED: usize = 0;
const CMA_KERNEL_OWNER: usize = usize::MAX;

struct CmaAllocator {
    owner: AtomicUsize,
    heap: UnsafeCell<Heap>,
}

// `heap` is accessed only while `owner` is held.  The guard is intentionally
// private so no caller can move it across a scheduling boundary.
unsafe impl Sync for CmaAllocator {}

impl CmaAllocator {
    const fn empty() -> Self {
        Self {
            owner: AtomicUsize::new(CMA_UNLOCKED),
            heap: UnsafeCell::new(Heap::empty()),
        }
    }

    fn owner_for_current_task() -> usize {
        crate::task::current_task()
            .map(|task| {
                task.tid()
                    .checked_add(1)
                    .filter(|owner| *owner != CMA_KERNEL_OWNER)
                    .expect("CMA task id cannot encode lock owner")
            })
            .unwrap_or(CMA_KERNEL_OWNER)
    }

    fn lock(&self) -> CmaGuard<'_> {
        let owner = Self::owner_for_current_task();
        while self
            .owner
            .compare_exchange(CMA_UNLOCKED, owner, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        CmaGuard {
            allocator: self,
            owner,
        }
    }

    fn with_heap<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> R {
        let guard = self.lock();
        guard.with_heap(f)
    }

    /// Task teardown discards its kernel stack instead of unwinding it.  A
    /// CMA guard on that stack therefore needs an explicit release point.
    /// This is safe only after `exit_current_and_run_next()` has detached the
    /// task, which guarantees the guarded code cannot resume.
    fn cancel_task(&self, tid: usize) {
        let owner = tid
            .checked_add(1)
            .filter(|owner| *owner != CMA_KERNEL_OWNER)
            .expect("CMA task id cannot encode lock owner");
        if self
            .owner
            .compare_exchange(owner, CMA_UNLOCKED, Ordering::Release, Ordering::Relaxed)
            .is_ok()
        {
            log::warn!("releasing CMA lock abandoned by exiting tid {}", tid);
        }
    }
}

struct CmaGuard<'a> {
    allocator: &'a CmaAllocator,
    owner: usize,
}

impl CmaGuard<'_> {
    fn with_heap<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> R {
        // SAFETY: `CmaGuard` is constructed only after the acquire operation
        // above succeeds, and its Drop releases the owner exactly once.
        unsafe { f(&mut *self.allocator.heap.get()) }
    }
}

impl Drop for CmaGuard<'_> {
    fn drop(&mut self) {
        self.allocator
            .owner
            .compare_exchange(
                self.owner,
                CMA_UNLOCKED,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .expect("CMA lock released by non-owner");
    }
}

static CMA_ALLOCATOR: CmaAllocator = CmaAllocator::empty();

/// Bound the work done by a 4 KiB CMA recycle.  DMA descriptor pages are
/// short-lived and can be reused from their existing size class; an
/// unbounded search for a merge partner must not stall a completed I/O.
const CMA_PAGE_BUDDY_SCAN_LIMIT: usize = 256;

/// initiate heap allocator
pub fn init_cma() {
    extern "C" {
        fn ekernel();
    }
    assert!(ekernel as *const () as usize % 4096 == 0);
    assert!(crate::arch::hardware::ram_range_count() != 0);

    println!("init_cma:");
    let ekernel_va = ekernel as *const () as usize;
    let total = init_cma_heap(ekernel_va);
    assert!(total > 0);

    // println!(
    //     "CMA allocator initialized: start = {:#x}, size = {:#x}",
    //     KERNEL_ADDR_OFFSET + crate::arch::hardware::ram_start(),
    //     total
    // );
    return;
}

#[cfg(target_arch = "loongarch64")]
fn init_cma_heap(ekernel_va: usize) -> usize {
    let mut total = 0usize;
    CMA_ALLOCATOR.with_heap(|allocator| {
        for index in 0..crate::arch::hardware::ram_range_count() {
            let (start, size) = crate::arch::hardware::ram_range(index)
                .expect("RAM range count changed during boot");
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
    });
    total
}

#[cfg(target_arch = "riscv64")]
fn init_cma_heap(ekernel_va: usize) -> usize {
    // Keep early allocator metadata inside the first GiB.  entry.asm also
    // maps the firmware FDT leaf when necessary, but the full RAM direct map
    // is still installed by mm::activate_kernel_space().
    assert_eq!(crate::arch::hardware::ram_range_count(), 1);
    let (physical_start, physical_size) =
        crate::arch::hardware::ram_range(0).expect("RISC-V bootloader did not provide RAM");
    let used_physical_memory = (ekernel_va - physical_start) - KERNEL_ADDR_OFFSET;
    let bootstrap_size = BOOTSTRAP_PHYSICAL_MEMORY_SIZE.min(physical_size);
    assert!(used_physical_memory < bootstrap_size);
    let size = bootstrap_size - used_physical_memory;
    let left = KERNEL_ADDR_OFFSET + physical_start + used_physical_memory;
    println!("from: {:#x}", left);
    println!("size: {:#x}", size);
    println!("to:   {:#x}", left + size);
    CMA_ALLOCATOR.with_heap(|allocator| unsafe {
        allocator.init(
            left, // 其实就是ekernel???
            size,
        );
    });
    size
}

/// Add RISC-V RAM that is inaccessible through the bootstrap page table.
///
/// This must run only after `activate_kernel_space()` has installed the full
/// direct map, because `add_to_heap()` writes free-list links into this range.
#[cfg(target_arch = "riscv64")]
pub fn init_cma_late() {
    assert_eq!(crate::arch::hardware::ram_range_count(), 1);
    let (physical_start, physical_size) =
        crate::arch::hardware::ram_range(0).expect("RISC-V bootloader did not provide RAM");
    let bootstrap_size = BOOTSTRAP_PHYSICAL_MEMORY_SIZE.min(physical_size);
    assert!(bootstrap_size <= physical_size);
    let start = KERNEL_ADDR_OFFSET + physical_start + bootstrap_size;
    let end = KERNEL_ADDR_OFFSET + physical_start + physical_size;
    println!("init_cma_late:");
    println!("from: {:#x}", start);
    println!("size: {:#x}", end - start);
    println!("to:   {:#x}", end);
    CMA_ALLOCATOR.with_heap(|allocator| unsafe { allocator.add_to_heap(start, end) });
}

#[cfg(not(target_arch = "riscv64"))]
pub fn init_cma_late() {}

/// Allocate a physically contiguous page range with an explicit alignment.
pub fn cma_alloc_aligned(pages: usize, align_pages: usize) -> Option<PhysAddr> {
    if pages == 0 || align_pages == 0 {
        return None;
    }
    let size = pages.checked_mul(PAGE_SIZE)?;
    let align = align_pages.checked_mul(PAGE_SIZE)?;
    let layout_opt = Layout::from_size_align(size, align).ok();
    match layout_opt {
        Some(layout) => {
            let ptr_result = match CMA_ALLOCATOR.with_heap(|allocator| allocator.alloc(layout)) {
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

/// 分配连续物理内存页（返回起始物理地址）。
pub fn cma_alloc(pages: usize) -> Option<PhysAddr> {
    cma_alloc_aligned(pages, 1)
}

/// 释放连续物理内存
pub fn cma_dealloc_aligned(paddr: PhysAddr, pages: usize, align_pages: usize) {
    assert!(pages > 0, "cannot deallocate an empty CMA range");
    assert_eq!(paddr.0 % PAGE_SIZE, 0);
    let layout = Layout::from_size_align(
        pages
            .checked_mul(PAGE_SIZE)
            .expect("CMA deallocation size overflow"),
        align_pages
            .checked_mul(PAGE_SIZE)
            .expect("CMA deallocation alignment overflow"),
    )
    .expect("Invalid deallocation layout");
    let va = KernelAddr::from(paddr);
    let ptr = NonNull::new(va.0 as *mut u8).expect("Pointer must not be null!");
    CMA_ALLOCATOR.with_heap(|allocator| {
        if pages == 1 {
            // VirtIO descriptors and FrameTracker pages are recycled at page
            // granularity. Heap::dealloc() scans an entire order-12 intrusive
            // list looking for a buddy; BuildStorm can make that list large
            // enough for one completed 4 KiB DMA request to busy-loop for
            // minutes. Keep ordinary coalescing, but cap the search. When the
            // cap is hit the page remains immediately reusable at order 12;
            // multi-page DMA ranges retain the full coalescing path below.
            allocator.dealloc_with_bounded_merge(ptr, layout, CMA_PAGE_BUDDY_SCAN_LIMIT);
        } else {
            allocator.dealloc(ptr, layout);
        }
    });
}

/// Release a contiguous range allocated with [`cma_alloc_aligned`].
pub fn cma_dealloc(paddr: PhysAddr, pages: usize) {
    cma_dealloc_aligned(paddr, pages, 1)
}

/// Release a CMA critical section that belongs to a task whose kernel stack is
/// about to be abandoned by task exit.
pub(crate) fn cancel_cma_lock_owner(tid: usize) {
    CMA_ALLOCATOR.cancel_task(tid);
}
