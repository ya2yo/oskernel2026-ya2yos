//! 物理页帧所有权与生命周期管理。
//!
//! [`FrameTracker`] 是一个物理页的 RAII 所有权令牌：创建时清零对应物理页，
//! 最后一个引用释放时将普通物理页归还 [`PAGE_CACHE`]。物理页通常以
//! [`Arc`] 形式在页表、VMA、页缓存以及缺页处理路径之间共享，因此真正的
//! 释放时机由最后一个 `Arc<FrameTracker>` 决定。
//!
//! 对于大页映射，多个 `FrameTracker` 可以分别代表同一连续大页中的不同
//! 4 KiB 子页，但它们共同持有一个 [`HugeFrameBlock`]。这样既能兼容现有的
//! 逐页 VMA 管理，又能保证底层连续内存只在整个大页块的最后一个引用释放
//! 时，使用原始的起始地址、页数和对齐约束一次性归还。

use super::page_cache::PAGE_CACHE;
use crate::mm::{PhysAddr, PhysPageNum};
use alloc::sync::Arc;
use core::fmt::{self, Debug, Formatter};

/// 一个物理连续大页块的所有权令牌。
///
/// 大页映射为了兼容现有 VMA 管理，仍然为每个虚拟页保留一个 4 KiB 的
/// `FrameTracker`；但是这些 tracker 共同持有本令牌。因此，底层原始的 CMA
/// 连续分配只会在最后一个引用释放时，按照原始布局精确归还一次。
///
/// `base_ppn`、`pages` 与 `align_pages` 必须与创建 CMA 分配时使用的参数
/// 完全一致。释放时不能根据某个子页 tracker 的 PPN 推导大页块起始位置，
/// 所以这些信息必须由令牌完整保存。
pub(crate) struct HugeFrameBlock {
    base_ppn: PhysPageNum,
    pages: usize,
    align_pages: usize,
}

impl HugeFrameBlock {
    /// 创建一个共享的大页块所有权令牌。
    ///
    /// 返回的 `Arc` 会被该大页中的每个子页 tracker 克隆。只有最后一个
    /// 引用销毁时，`Drop` 实现才会释放整个连续 CMA 分配。
    pub(crate) fn new(base_ppn: PhysPageNum, pages: usize, align_pages: usize) -> Arc<Self> {
        Arc::new(Self {
            base_ppn,
            pages,
            align_pages,
        })
    }
}

impl Drop for HugeFrameBlock {
    /// 归还整个物理连续大页块，而不是单独释放某个子页。
    fn drop(&mut self) {
        crate::mm::cma_dealloc_aligned(PhysAddr::from(self.base_ppn), self.pages, self.align_pages);
    }
}

/// 一个物理页的 RAII 生命周期管理器。
///
/// `ppn` 是该 tracker 管理的物理页号。普通页的释放路径是将它交还给
/// [`PAGE_CACHE`]；属于大页块的子页则通过 `huge_block` 间接持有整个连续
/// 分配，避免把大页错误地拆成多个独立 CMA 页来释放。
///
/// 该类型本身不表示页表映射，也不负责设置访问权限；它只负责物理帧的
/// 所有权、初始化和释放。由于它通常被 `Arc` 包装，复制 `Arc` 只会增加
/// 所有权引用，不会复制物理页内容。
pub struct FrameTracker {
    pub ppn: PhysPageNum,
    huge_block: Option<Arc<HugeFrameBlock>>,
}

impl FrameTracker {
    /// 从物理页号构造 tracker，并将整页内容清零。
    ///
    /// 清零发生在所有公开构造路径的共同底层函数中，确保新分配页、外部
    /// 传入页以及大页子页在交给调用者前都不会泄露此前的内存内容。该函数
    /// 不负责验证 `ppn` 是否来自可分配内存区域；调用者必须保证物理页号
    /// 有效且由当前分配策略独占或正确管理。
    fn new(ppn: PhysPageNum) -> Self {
        let bytes_array = ppn.bytes_array_mut();
        for i in bytes_array {
            *i = 0;
        }
        Self {
            ppn,
            huge_block: None,
        }
    }

    /// 从全局页缓存中分配一个物理页。
    ///
    /// 分配成功时返回持有该页所有权的 `Arc<FrameTracker>`；如果页缓存及其
    /// 后备 CMA 分配都无法提供物理页，则返回 `None`。新页会在返回前清零，
    /// 以避免调用者观察到上一个使用者留下的数据。
    pub fn alloc() -> Option<Arc<FrameTracker>> {
        let ppn = PAGE_CACHE.alloc()?;
        let ret = Some(Arc::new(FrameTracker::new(ppn)));
        ret
    }

    /// 接管一个已经选定的物理页，并在交给调用者前清零。
    ///
    /// 此方法不会再次向 `PAGE_CACHE` 申请页面，因此调用者必须确保该物理页
    /// 已经从其他分配路径中正确取得，并且不会同时由另一个所有者管理。
    pub(crate) fn from_ppn(ppn: PhysPageNum) -> Arc<FrameTracker> {
        Arc::new(FrameTracker::new(ppn))
    }

    /// 将对齐大页分配中的一个子页包装为 tracker。
    ///
    /// `ppn` 可以是大页块中的任意一个 4 KiB 子页；`block` 必须是与该页
    /// 对应的共享大页块令牌。构造过程同样会清零该子页。tracker 销毁时不会
    /// 将该子页单独放回页缓存，而是通过持有 `block` 延长整个大页块的生命期。
    pub(crate) fn from_huge_ppn(ppn: PhysPageNum, block: Arc<HugeFrameBlock>) -> Arc<FrameTracker> {
        let mut frame = Self::new(ppn);
        frame.huge_block = Some(block);
        Arc::new(frame)
    }
}

impl Debug for FrameTracker {
    /// 以简洁的物理页号形式输出 tracker，便于日志和调试器查看。
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("FrameTracker:PPN={:#x}", self.ppn.0))
    }
}

impl Drop for FrameTracker {
    /// 在 tracker 的最后一个所有权引用释放时归还普通物理页。
    ///
    /// 大页子页不在此处单独释放；它们通过 `huge_block` 共享所有权，并由
    /// `HugeFrameBlock::drop` 一次性归还完整的连续 CMA 分配。
    fn drop(&mut self) {
        if self.huge_block.is_none() {
            PAGE_CACHE.dealloc(self.ppn);
        }
    }
}
