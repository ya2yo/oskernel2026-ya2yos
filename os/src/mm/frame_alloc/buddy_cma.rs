//! # CMA（Contiguous Memory Allocator）：基于伙伴算法的连续物理地址分配器
//!
//! 除了内核自身的各 ELF 段（内核堆位于数据段内部）之外，其余所有空闲物理
//! 内存都交给本模块管理。上层通过 [`cma_alloc_aligned`] / [`cma_dealloc_aligned`]
//! 申请/归还物理连续的内存块，服务对象包括 VirtIO / GMAC 等 DMA 驱动、
//! 大页映射（[`crate::mm::map_area`]）以及内核堆扩容
//! （[`crate::mm::heap_allocator`]）。
//!
//! 分配器在多个 hart 的任务之间共享，因此需要一把自旋锁。**不能直接使用
//! `LockedHeap`**：它依赖的 spin 0.7 是 ticket lock——任务在取得 ticket 后
//! 若被调度器摘除，会把所有后续的分配调用者永久卡死。任务退出会丢弃内核栈
//! 而不是展开它，通常的 RAII 解锁在这里无法兜底。本模块改用带 owner 的原子
//! 自旋锁（见 [`CmaAllocator`]），并在任务退出路径上显式调用
//! [`cancel_cma_lock_owner`] 释放被遗弃的临界区。
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

/// 锁空闲标记：`owner == CMA_UNLOCKED` 表示没有任务持有该锁，任何调用者都
/// 可以用一次 `compare_exchange` 原子抢占。
const CMA_UNLOCKED: usize = 0;

/// 内核上下文（启动阶段、中断处理等没有当前任务的路径）对应的锁 owner。
///
/// 该值刻意取 `usize::MAX`，与 `tid + 1` 的编码区间不相交，因此内核 owner
/// 永远不会和某个任务 owner 混淆。
const CMA_KERNEL_OWNER: usize = usize::MAX;

/// 全局 CMA 分配器：把无锁的 [`buddy_system_allocator::Heap`] 与一把 owner
/// 自旋锁组合在一起，保证任意时刻至多一个任务访问堆。
///
/// 锁约定：
/// - `owner == CMA_UNLOCKED` 时空闲；否则被某个 owner（`tid + 1` 或
///   [`CMA_KERNEL_OWNER`]）持有，其余调用者自旋等待。
/// - 只有持锁者本人能在 Drop 时把 owner 放回 [`CMA_UNLOCKED`]。
///
/// `heap` 仅在持有 owner 期间被访问；`CmaGuard` 被刻意设计为私有类型，
/// 使调用者无法把它带过调度边界（任务被摘除时其内核栈会被整体丢弃）。
struct CmaAllocator {
    owner: AtomicUsize,
    heap: UnsafeCell<Heap>,
}

// `heap` is accessed only while `owner` is held.  The guard is intentionally
// private so no caller can move it across a scheduling boundary.
unsafe impl Sync for CmaAllocator {}

impl CmaAllocator {
    /// 构造未绑定任何内存的空分配器，供静态初始化使用。
    const fn empty() -> Self {
        Self {
            owner: AtomicUsize::new(CMA_UNLOCKED),
            heap: UnsafeCell::new(Heap::empty()),
        }
    }

    /// 计算当前上下文的锁 owner。
    ///
    /// 存在当前任务时返回 `tid + 1`——加一是为了避免 tid 0 与
    /// [`CMA_UNLOCKED`] 冲突；没有当前任务（启动阶段、中断上下文）返回
    /// [`CMA_KERNEL_OWNER`]。tid 有界，溢出或撞上 [`CMA_KERNEL_OWNER`]
    /// 属于不可能发生的不变量，违反即 panic。
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

    /// 自旋获取分配器锁，成功后返回 [`CmaGuard`]。
    ///
    /// 使用 `Acquire` 语义的 `compare_exchange` 抢占；失败则自旋重试。
    /// 与 ticket lock 不同，这里没有需要排队取号的顺序，因此被摘除的任务
    /// 不可能挡住后续调用者——最坏情况只是被其遗弃的临界区占用到任务退出
    /// 路径显式回收为止。
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

    /// 加锁后以闭包形式访问底层 [`Heap`](buddy_system_allocator::Heap)。
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

/// 持有 CMA owner 锁的 RAII 守卫，是访问底层
/// [`Heap`](buddy_system_allocator::Heap) 的唯一入口。
///
/// 该类型是私有的：调用者只能经由 [`CmaAllocator::with_heap`] 在临界区内
/// 操作堆，无法把守卫保存进任何跨调度边界的数据结构。若持有临界区的任务被
/// 摘除（内核栈被丢弃，守卫的 Drop 不会执行），由任务退出路径上的
/// [`cancel_cma_lock_owner`] 代为解锁。
struct CmaGuard<'a> {
    allocator: &'a CmaAllocator,
    owner: usize,
}

impl CmaGuard<'_> {
    /// 在临界区内以独占的 `&mut Heap` 调用 `f`。
    ///
    /// # Safety
    ///
    /// 守卫只会在上面的 acquire 成功后才被构造，且其 Drop 恰好把 owner
    /// 释放回 [`CMA_UNLOCKED`] 一次，因此这里能安全地取得堆的可变引用。
    fn with_heap<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> R {
        // SAFETY: `CmaGuard` is constructed only after the acquire operation
        // above succeeds, and its Drop releases the owner exactly once.
        unsafe { f(&mut *self.allocator.heap.get()) }
    }
}

impl Drop for CmaGuard<'_> {
    /// 用 `Release` 语义释放 owner 锁；owner 不符说明锁状态损坏，直接 panic。
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

/// 全局唯一的 CMA 分配器实例。
///
/// 堆中保存的是物理内存经 direct map 后的内核虚拟地址；对外接口
/// （[`cma_alloc_aligned`] 等）返回去掉 [`KERNEL_ADDR_OFFSET`] 后的物理
/// 地址。
static CMA_ALLOCATOR: CmaAllocator = CmaAllocator::empty();

/// Bound the work done by a 4 KiB CMA recycle.  DMA descriptor pages are
/// short-lived and can be reused from their existing size class; an
/// unbounded search for a merge partner must not stall a completed I/O.
const CMA_PAGE_BUDDY_SCAN_LIMIT: usize = 256;

/// 初始化 CMA 分配器：把内核镜像结束（`ekernel`）之后的所有空闲 RAM 加入
/// 伙伴堆。必须在 `mm::init()` 流程的早期、任何分配请求之前调用一次。
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

/// 把各 RAM 区间中位于 `ekernel` 之后的部分依次加入伙伴堆，返回加入的总
/// 字节数。
///
/// 每个 RAM 区间 `[range_start, range_end)`（内核虚拟地址）中，只有
/// `max(range_start, ekernel_va)` 到 `range_end` 的空闲部分是自由的：内核
/// 镜像本身（含位于数据段内的堆元数据）不能被分配。区间整体位于内核之下时
/// 直接跳过。
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

/// RISC-V 版：只把 bootstrap 阶段映射的头 [`BOOTSTRAP_PHYSICAL_MEMORY_SIZE`]
/// 字节 RAM 中 `ekernel` 之后的部分加入伙伴堆，返回加入的字节数。
///
/// 启动初期只有 entry.asm 安装的 bootstrap 页表可访问前 1 GiB，伙伴堆的空闲
/// 链表链接也要写进这段内存，所以元数据必须落在这里面。RAM 中超出 bootstrap
/// 范围的其余部分要等 [`init_cma_late`] 在完整 direct map 建立后再加入。
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

/// 非 RISC-V 平台上，所有 RAM 已在 [`init_cma`] 时全部加入，无需后期补充。
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

/// 分配 `pages` 个物理连续的页面，返回起始物理地址。
///
/// 等价于 [`cma_alloc_aligned`]`(pages, 1)`（对齐到一页）。内存不足时返回
/// `None`。调用方负责通过 [`cma_dealloc`] 归还。
pub fn cma_alloc(pages: usize) -> Option<PhysAddr> {
    cma_alloc_aligned(pages, 1)
}

/// 释放一段物理连续内存，参数必须与当初 [`cma_alloc_aligned`] 时一致。
///
/// 单页（`pages == 1`）场景——VirtIO 描述符、FrameTracker 页等——走
/// [`buddy_system_allocator::Heap::dealloc_with_bounded_merge`]，把每次归还
/// 的伙伴查找限制在 [`CMA_PAGE_BUDDY_SCAN_LIMIT`] 以内，避免 BuildStorm 等
/// 高碎片场景下一次 4 KiB DMA 完成事件在 order-12 链表上长时间自旋；代价是
/// 偶发不合并，页面仍留在原 size class 中可立即复用。多页范围保留完整的
/// 合并路径。
///
/// # Panics
///
/// 参数非法（`pages == 0`、地址未页对齐、layout 溢出）时 panic。
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

/// 释放 [`cma_alloc`] 分配的一段物理连续内存（页对齐）。
pub fn cma_dealloc(paddr: PhysAddr, pages: usize) {
    cma_dealloc_aligned(paddr, pages, 1)
}

/// Release a CMA critical section that belongs to a task whose kernel stack is
/// about to be abandoned by task exit.
pub(crate) fn cancel_cma_lock_owner(tid: usize) {
    CMA_ALLOCATOR.cancel_task(tid);
}
