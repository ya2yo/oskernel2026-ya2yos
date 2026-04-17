// -------------PageCache------------

use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::vec::Vec;
use log::debug;
use spin::Mutex;

use crate::mm::{PhysAddr, PhysPageNum};

use super::{cma_alloc, cma_dealloc};

/// 单页缓存结构
pub struct PageCache {
    free_list: Mutex<Vec<PhysPageNum>>, // 空闲页链表
    count: AtomicUsize,                 // 当前缓存页数
}

impl PageCache {
    const HIGH_WATERMARK: usize = 128; // 缓存页数上限
    const LOW_WATERMARK: usize = 32; // 缓存页数下限
    const REFILL_BATCH: usize = 16; // 每次补充页数
    const FLUSH_BATCH: usize = 64; // 每次刷回页数

    /// 创建新缓存
    pub const fn new() -> Self {
        Self {
            free_list: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        }
    }

    /// 从缓存分配单页
    pub fn alloc(&self) -> Option<PhysPageNum> {
        let mut list = self.free_list.lock();
        loop {
            if let Some(ppn) = list.pop() {
                self.count.fetch_sub(1, Ordering::Relaxed);
                return Some(ppn);
            }

            // 缓存为空，批量补充
            for _ in 0..Self::REFILL_BATCH {
                let page = match cma_alloc(1) {
                    Some(addr) => PhysPageNum::from(addr),
                    None => return None,
                };
                list.push(page);
            }
            self.count.fetch_add(Self::REFILL_BATCH, Ordering::Relaxed);
        }
    }

    /// 释放单页到缓存
    pub fn dealloc(&self, ppn: PhysPageNum) {
        let mut list = self.free_list.lock();
        list.push(ppn);
        let count = self.count.fetch_add(1, Ordering::Relaxed) + 1;

        // 超过高水位线时刷回
        if count >= Self::HIGH_WATERMARK {
            if list.len() <= Self::LOW_WATERMARK {
                return;
            }
            // 收集待刷回的连续页（优化伙伴系统合并）
            let mut flush_pages = Vec::new();
            for _ in 0..Self::FLUSH_BATCH.min(list.len() - Self::LOW_WATERMARK) {
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
