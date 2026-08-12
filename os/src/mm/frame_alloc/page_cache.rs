// -------------PageCache------------

use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::vec::Vec;
use log::{debug, warn};
use spin::Mutex;

use crate::mm::{PhysAddr, PhysPageNum};
use crate::utils::SysErrNo;

use super::{cma_alloc, cma_dealloc};

/// 单页缓存结构
pub struct PageCache {
    free_list: Mutex<Vec<PhysPageNum>>, // 空闲页链表
    count: AtomicUsize,                 // 当前缓存页数
    /// Total frames allocated (for diagnostics)
    pub total_allocated: AtomicUsize,
    /// Total frames deallocated (for diagnostics)
    pub total_deallocated: AtomicUsize,
    // These values are intentionally atomic: the configuration is normally
    // loaded during boot, while allocator users may run concurrently later.
    high_watermark: AtomicUsize,
    low_watermark: AtomicUsize,
    refill_batch: AtomicUsize,
    flush_batch: AtomicUsize,
}

impl PageCache {
    pub const DEFAULT_HIGH_WATERMARK: usize = 128;
    pub const DEFAULT_LOW_WATERMARK: usize = 32;
    pub const DEFAULT_REFILL_BATCH: usize = 16;
    pub const DEFAULT_FLUSH_BATCH: usize = 64;
    pub const MAX_WATERMARK: usize = 65536;
    pub const MAX_BATCH: usize = 4096;

    /// 创建新缓存
    pub const fn new() -> Self {
        Self {
            free_list: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
            total_allocated: AtomicUsize::new(0),
            total_deallocated: AtomicUsize::new(0),
            high_watermark: AtomicUsize::new(Self::DEFAULT_HIGH_WATERMARK),
            low_watermark: AtomicUsize::new(Self::DEFAULT_LOW_WATERMARK),
            refill_batch: AtomicUsize::new(Self::DEFAULT_REFILL_BATCH),
            flush_batch: AtomicUsize::new(Self::DEFAULT_FLUSH_BATCH),
        }
    }

    pub fn config(&self) -> (usize, usize, usize, usize) {
        (
            self.high_watermark.load(Ordering::Relaxed),
            self.low_watermark.load(Ordering::Relaxed),
            self.refill_batch.load(Ordering::Relaxed),
            self.flush_batch.load(Ordering::Relaxed),
        )
    }

    pub fn configure(
        &self,
        high_watermark: usize,
        low_watermark: usize,
        refill_batch: usize,
        flush_batch: usize,
    ) -> Result<(), SysErrNo> {
        if low_watermark == 0
            || low_watermark > high_watermark
            || high_watermark > Self::MAX_WATERMARK
            || refill_batch == 0
            || flush_batch == 0
            || refill_batch > Self::MAX_BATCH
            || flush_batch > Self::MAX_BATCH
        {
            return Err(SysErrNo::EINVAL);
        }
        self.high_watermark.store(high_watermark, Ordering::Relaxed);
        self.low_watermark.store(low_watermark, Ordering::Relaxed);
        self.refill_batch.store(refill_batch, Ordering::Relaxed);
        self.flush_batch.store(flush_batch, Ordering::Relaxed);
        Ok(())
    }

    /// 从缓存分配单页
    pub fn alloc(&self) -> Option<PhysPageNum> {
        let mut list = self.free_list.lock();
        let (_, _, refill_batch, _) = self.config();
        loop {
            if let Some(ppn) = list.pop() {
                self.count.fetch_sub(1, Ordering::Relaxed);
                self.total_allocated.fetch_add(1, Ordering::Relaxed);
                return Some(ppn);
            }

            // 缓存为空，批量补充
            for _ in 0..refill_batch {
                let page = match cma_alloc(1) {
                    Some(addr) => PhysPageNum::from(addr),
                    None => {
                        let total = self.total_allocated.load(Ordering::Relaxed);
                        let freed = self.total_deallocated.load(Ordering::Relaxed);
                        warn!(
                            "CMA OOM! total_allocated={}, total_freed={}, in_use={}",
                            total,
                            freed,
                            total.saturating_sub(freed)
                        );
                        return None;
                    }
                };
                list.push(page);
            }
            self.count.fetch_add(refill_batch, Ordering::Relaxed);
        }
    }

    /// 释放单页到缓存
    pub fn dealloc(&self, ppn: PhysPageNum) {
        let mut list = self.free_list.lock();
        list.push(ppn);
        self.total_deallocated.fetch_add(1, Ordering::Relaxed);
        let count = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        let (high_watermark, low_watermark, _, flush_batch) = self.config();

        // 超过高水位线时刷回
        if count >= high_watermark {
            if list.len() <= low_watermark {
                return;
            }
            // 收集待刷回的连续页（优化伙伴系统合并）
            let mut flush_pages = Vec::new();
            for _ in 0..flush_batch.min(list.len() - low_watermark) {
                flush_pages.push(list.pop().unwrap());
            }
            self.count.fetch_sub(flush_pages.len(), Ordering::Relaxed);
            drop(list); // 提前释放锁

            for i in flush_pages {
                crate::mm::cma_dealloc(PhysAddr::from(i), 1);
            }
        }
    }
}

// 全局页缓存实例
pub static PAGE_CACHE: PageCache = PageCache::new();
