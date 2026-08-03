use core::{
    alloc::Layout,
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use buddy_system_allocator::Heap;

#[cfg(target_arch = "riscv64")]
use crate::arch::memory_layout::BOOTSTRAP_PHYSICAL_MEMORY_SIZE;
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
    CMA_ALLOCATOR.with_heap(|allocator| {
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
    });
    total
}

#[cfg(target_arch = "riscv64")]
fn init_cma_heap(ekernel_va: usize) -> usize {
    // entry.asm only maps the first 1GiB before mm::activate_kernel_space().
    // Buddy free-list nodes are stored in the managed range itself, so adding
    // the second GiB here would touch an address that is not mapped yet.
    let used_physical_memory = (ekernel_va - PHYSICAL_MEMORY_START) - KERNEL_ADDR_OFFSET;
    assert!(used_physical_memory < BOOTSTRAP_PHYSICAL_MEMORY_SIZE);
    let size = BOOTSTRAP_PHYSICAL_MEMORY_SIZE - used_physical_memory;
    let left = KERNEL_ADDR_OFFSET + PHYSICAL_MEMORY_START + used_physical_memory;
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
    assert!(BOOTSTRAP_PHYSICAL_MEMORY_SIZE <= PHYSICAL_MEMORY_SIZE);
    let start = KERNEL_ADDR_OFFSET + PHYSICAL_MEMORY_START + BOOTSTRAP_PHYSICAL_MEMORY_SIZE;
    let end = KERNEL_ADDR_OFFSET + PHYSICAL_MEMORY_START + PHYSICAL_MEMORY_SIZE;
    println!("init_cma_late:");
    println!("from: {:#x}", start);
    println!("size: {:#x}", end - start);
    println!("to:   {:#x}", end);
    CMA_ALLOCATOR.with_heap(|allocator| unsafe { allocator.add_to_heap(start, end) });
}

#[cfg(not(target_arch = "riscv64"))]
pub fn init_cma_late() {}

/// 分配连续物理内存页（返回起始物理地址）
pub fn cma_alloc(pages: usize) -> Option<PhysAddr> {
    let layout_opt = Layout::from_size_align(
        pages * PAGE_SIZE,
        PAGE_SIZE, // 按页对齐
    )
    .ok();
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

/// 释放连续物理内存
pub fn cma_dealloc(paddr: PhysAddr, pages: usize) {
    assert_eq!(paddr.0 % PAGE_SIZE, 0);
    let layout =
        Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).expect("Invalid deallocation layout");
    let va = KernelAddr::from(paddr);
    let ptr = NonNull::new(va.0 as *mut u8).expect("Pointer must not be null!");
    CMA_ALLOCATOR.with_heap(|allocator| allocator.dealloc(ptr, layout));
}

/// Release a CMA critical section that belongs to a task whose kernel stack is
/// about to be abandoned by task exit.
pub(crate) fn cancel_cma_lock_owner(tid: usize) {
    CMA_ALLOCATOR.cancel_task(tid);
}
