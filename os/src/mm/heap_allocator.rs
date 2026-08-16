//! 内核全局堆分配器。
//!
//! 本模块为 Rust 的 [`alloc`] 类型提供全局分配入口。堆的主要后端是
//! [`LockedHeap`]，初始内存来自内核 BSS 中预留的静态区域；内存管理子系统
//! 初始化完成、CMA 的直接映射可用后，分配失败时还可以从 CMA 借用连续物理
//! 页面扩展堆。
//!
//! CMA 转入全局堆的内存一旦通过 [`LockedHeap::add_to_heap`] 移交，就由
//! buddy allocator 统一管理，后续不能再交还给 CMA。分配器锁保护 buddy
//! 空闲链表，CMA 扩容锁则串行化“从 CMA 取内存并加入全局堆”的过程。
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
/// Rust `alloc` 使用的内核全局分配器入口。
///
/// 实际分配先尝试 [`HEAP`]；失败后由 [`alloc_from_heap`] 尝试从 CMA
/// 扩展 buddy heap。释放操作始终归还给 [`HEAP`]，因为扩展后的内存所有权
/// 已经从 CMA 转移到全局堆。
static HEAP_ALLOCATOR: KernelHeapAllocator = KernelHeapAllocator;

/// 支撑 Rust 全局分配器的 buddy heap。
///
/// 初始时为空，由 [`init_heap`] 加入 BSS 静态堆区域；内存管理子系统完成
/// 初始化后，还可以通过 [`grow_from_cma`] 加入 CMA 提供的扩展区域。
static HEAP: LockedHeap = LockedHeap::empty();

/// 控制 CMA 是否已经完成全局堆扩容所需的初始化。
///
/// 在该标志设置前，分配失败路径不能访问 CMA，避免使用尚未建立直接映射
/// 或尚未完成初始化的物理内存范围。
static CMA_HEAP_BACKING_READY: AtomicBool = AtomicBool::new(false);

/// 串行化从 CMA 获取内存并加入全局 heap 的操作。
///
/// 获取该锁前会释放 `HEAP` 锁，避免在 CMA 分配过程中同时持有两个锁。
static CMA_HEAP_GROW_LOCK: Mutex<()> = Mutex::new(());

/// 已从 CMA 永久转入全局 kernel heap 的字节数。
///
/// 该值用于诊断和分配失败日志，不代表当前仍然空闲的 CMA 容量。
static CMA_HEAP_BACKING_BYTES: AtomicUsize = AtomicUsize::new(0);

/// 限制持有全局分配器锁时扫描 buddy 空闲链表的工作量。
///
/// 大量短生命周期元数据对象可能导致释放操作在空闲链表上进行长时间扫描。
/// 达到上限时可以暂不合并伙伴块，但当前块仍会留在所属阶中并可立即复用；
/// 真正需要更大块时仍可通过 CMA 扩容解决。
const HEAP_BUDDY_SCAN_LIMIT: usize = 256;

/// 堆压力恢复时优先尝试获取的较大 CMA 区间大小。
///
/// 如果 CMA 无法提供该大小的连续区间，扩容逻辑会逐步减半，直到退回到
/// 当前请求所需的最小 buddy block。
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

/// 计算满足一次 CMA 扩容所需的最小字节数。
///
/// buddy allocator 要求块大小为 2 的幂，因此先将请求大小向上取整；同时
/// 还要满足布局的对齐要求和页大小要求，保证 CMA 交出的范围能够按页完整
/// 管理且不会产生重叠或未归属的尾部空间。
fn required_cma_bytes(layout: Layout) -> Option<usize> {
    let requested = layout.size().checked_next_power_of_two()?;
    Some(max(max(requested, layout.align()), PAGE_SIZE))
}

/// 先从当前 buddy heap 分配；失败时再尝试从 CMA 扩容。
///
/// 将这层逻辑独立于 [`GlobalAlloc`] trait 实现之外，既可供全局分配器使用，
/// 也可让需要页对齐布局的 [`ContinuousPages`] 共享相同的 CMA 回退策略。
fn alloc_from_heap(layout: Layout) -> Result<NonNull<u8>, ()> {
    let first_attempt = { HEAP.lock().alloc(layout) };
    match first_attempt {
        Ok(ptr) => Ok(ptr),
        Err(_) => grow_from_cma(layout),
    }
}

/// 从 CMA 获取一段连续物理内存并将其并入全局 buddy heap。
///
/// CMA 尚未准备好时不会触碰 CMA。扩容期间先持有 [`CMA_HEAP_GROW_LOCK`]，
/// 并在加锁后再次尝试原始请求，避免其他 hart 已经扩容成功时重复取页。
/// 新范围加入 buddy heap 后立即预留当前请求，防止并发分配者抢走刚扩展的
/// 空间。首选较大的扩容块；若 CMA 碎片化，则逐步减半，直到退回到请求所需
/// 的最小 buddy 块。
///
/// 成功后，`grow_size` 对应的所有页面都归全局堆所有，不能再调用
/// [`crate::mm::cma_dealloc`] 归还 CMA。
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
/// 记录当前堆的统计信息并终止内核执行。
///
/// 到达此处理器时，当前请求已经无法从现有 buddy heap 或 CMA 扩容中满足。
/// 统计值同时包含用户请求字节数、buddy allocator 实际占用字节数、堆总容量
/// 以及从 CMA 永久转入堆的字节数，便于定位碎片化或容量不足问题。
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

/// 内核静态初始堆的存储区域。
///
/// `repr(align(4096))` 确保起始地址满足页对齐要求，从而可以直接交给
/// buddy heap 初始化。该区域位于 BSS 中，生命周期覆盖整个内核运行期。
#[repr(align(4096))]
struct HeapSpace([u8; KERNEL_HEAP_SIZE]);

/// 静态初始堆的实际存储空间。
///
/// `SyncUnsafeCell` 提供全局分配器所需的内部可变性；并发访问由 [`HEAP`]
/// 的锁保护，不能绕过分配器锁直接读写该区域。
static HEAP_SPACE: SyncUnsafeCell<HeapSpace> =
    SyncUnsafeCell::new(HeapSpace([0; KERNEL_HEAP_SIZE]));

/// 初始化全局 buddy heap，使其管理静态初始堆区域。
///
/// 应在内核开始使用 [`alloc`] 分配之前调用；重复初始化会破坏分配器状态，
/// 因此初始化顺序由内存管理子系统负责保证。
pub fn init_heap() {
    unsafe {
        HEAP.lock()
            .init(HEAP_SPACE.get() as usize, KERNEL_HEAP_SIZE);
    }
}

/// 标记 CMA 已完成架构相关初始化，可以作为全局堆的扩容后端。
///
/// 调用方必须保证此时 CMA 范围已经建立可用的内核直接映射；在此之前，
/// 分配失败只能返回错误，不能尝试从 CMA 取页。
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

/// 分配并封装一段连续、页对齐的内核内存。
///
/// `page_num` 是请求的页数；返回对象拥有这段内存，并在销毁时将其释放回
/// 全局 buddy heap。这里使用与全局分配器相同的 CMA 扩容回退，因此即使
/// 初始静态堆不足，也不会静默绕过统一的堆所有权管理。
pub struct ContinuousPages {
    base: usize,
    page_num: usize,
    layout: Layout,
}

unsafe impl Send for ContinuousPages {}
unsafe impl Sync for ContinuousPages {}

impl ContinuousPages {
    /// 分配 `page_num` 个连续的页对齐页面。
    ///
    /// 页数乘法、布局创建和底层分配任一步失败都会返回 `None`。返回成功
    /// 后，内存由 [`ContinuousPages`] 持有，调用 [`Self::base`] 可取得起始
    /// 地址；调用方不应手动释放或重复使用该地址对应的分配布局。
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

    /// 返回这段连续页面的起始虚拟地址。
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
