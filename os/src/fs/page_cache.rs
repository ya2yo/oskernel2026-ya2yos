//! 文件页缓存。
//!
//! 本模块为普通文件的 `read`、`mmap` 和 `splice` 路径提供共享的页级缓存。缓存以
//! `(文件路径, 页号)` 为键保存页帧及其有效长度，并通过引用计数保护正在使用的页。
//! 缺页时由调用者从 inode 读取数据；成功加载的页可以被后续读操作和文件映射复用。
//!
//! 缓存容量由全局页数上限控制。容量不足时，模块使用带延迟队列的 CLOCK 策略回收
//! 未被引用的干净页；脏页、仍被其他对象引用的页和刚刚访问过的页不会被回收。
//! 本模块只负责缓存页的查找、加载、预读和失效，不负责把脏页写回文件。

use alloc::{
    collections::{btree_map::Entry, BTreeMap, VecDeque},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::{Mutex, RwLock};

use crate::{arch::memory_layout::PAGE_SIZE, fs::Inode, mm::FrameTracker, utils::SysErrNo};

/// 一次确认的顺序缺页最多通过一次文件系统读取加载的相邻页数。
///
/// 该值有意保持较小：全局缓存容量固定，且一次预读不能绕过容量限制；对常见的
/// ELF 和源文件 mmap 遍历来说，16 KiB 已足以摊薄 lwext4 锁和文件读取的开销。
const SEQUENTIAL_READAHEAD_MAX_PAGES: usize = 4;

/// 限制一次 CLOCK 活跃队列扫描的步数。
///
/// 一次容量压力事件不必遍历整个缓存；保留队列位置可以让后续事件从本次扫描停止
/// 的位置继续处理。
const EVICTION_SCAN_BUDGET: usize = 128;

/// 当已知候选页全部为脏页或仍被缓存外对象引用时，经过有限次数的容量失败后，
/// 只重新尝试其中一小批页面。
///
/// 这样可以避免 mmap 密集型工作负载为了每个冷页反复扫描整个被固定的工作集。
const EVICTION_DEFERRED_RETRY_BATCH: usize = 16;
/// 延迟队列重新尝试前需要累计的容量失败次数。
const EVICTION_DEFERRED_RETRY_MISS_INTERVAL: usize = 256;

/// 限制全局文件页缓存的大小，避免长期运行的编译器遍历大型源文件和构建产物时
/// 无限占用内存。
///
/// 当前页大小为 4 KiB 时，192K 页约为 768 MiB。该上限是保守的内存预算，而不是
/// 用于运行时调优的参数；达到上限后会优先回收未使用的干净页。
#[cfg(not(feature = "file-cache-capacity-test"))]
const MAX_FILE_PAGE_CACHE_PAGES: usize = 192 * 1024;

/// 定向 QEMU 回归测试使用的较小缓存容量。
///
/// 该容量为启动 Bash 留出空间，同时无需执行数 GiB 的 BuildStorm 编译即可暴露
/// mmap 绕过容量限制的路径。普通构建不会启用此特性。
#[cfg(feature = "file-cache-capacity-test")]
const MAX_FILE_PAGE_CACHE_PAGES: usize = 2 * 1024;

/// 文件页缓存的索引键。
///
/// 同一个文件由路径唯一标识，文件内容按 `PAGE_SIZE` 切分后使用
/// `page_index` 定位具体页。
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct FilePageKey {
    /// 被缓存文件的路径。
    pub path: Arc<str>,
    /// 文件内以 `PAGE_SIZE` 为单位的页号。
    pub page_index: usize,
}

/// 文件页缓存中的单个页。
///
/// `frame` 保存实际页数据，`valid_len` 表示该页从文件中读出的有效字节数。
/// 当页内容被写入后，需要通过 `mark_dirty` 标记脏页，以便后续回写或失效逻辑识别。
pub struct FilePage {
    /// 当前缓存页对应的文件路径和页号。
    pub key: FilePageKey,
    /// 承载页内容的物理页帧。
    pub frame: Arc<FrameTracker>,
    /// 当前页中来自文件的有效字节数。
    ///
    /// 文件尾页可能小于 `PAGE_SIZE`，超出 `valid_len` 的部分在加载时会被清零。
    pub valid_len: usize,
    /// 页内容是否已经被修改但尚未同步到底层文件。
    dirty: AtomicBool,
    /// CLOCK 算法的访问标记。
    ///
    /// 缓存查找无需获取缓存写锁即可设置该标记；缓存扫描会先清除标记，若下一次
    /// 扫描前页面再次被访问，则给予该页一次“第二次机会”，暂不回收。
    referenced: AtomicBool,
}

/// 同一路径下的所有缓存页。
///
/// 将页号放在路径下的第二层映射中，可以避免 read 或 mmap 连续探测同一文件的
/// 相邻页面时反复比较路径字符串。
type FilePages = BTreeMap<usize, Arc<FilePage>>;

/// 持有页缓存写锁检查一个 CLOCK 候选页后的结果。
///
/// 该枚举不保存页引用，因此回收判断不会因为检查过程本身改变正在检查的引用计数。
enum EvictionState {
    /// 页面满足回收条件。
    Evict,
    /// 页面最近被访问，应保留到下一轮扫描。
    SecondChance,
    /// 页面包含尚未回写的数据。
    Dirty,
    /// 页面仍被缓存外对象使用。
    InUse,
}

/// 干净页回收使用的候选队列。
///
/// 每个驻留页的键恰好位于一个队列中：`active` 参与普通 CLOCK 扫描，`deferred`
/// 保存最近发现为脏页或仍被缓存外对象引用的页面。
///
/// 只有 `active` 为空时才检查 `retry_misses`。它统计的是缓存压力导致的失败次数，
/// 而不是时钟 tick，因此 mmap 释放后无需定时器或后台线程即可让延迟候选页重新参与
/// 回收。
struct EvictionQueues {
    /// 正常 CLOCK 扫描队列。
    active: VecDeque<FilePageKey>,
    /// 暂时不适合扫描的候选页队列。
    deferred: VecDeque<FilePageKey>,
    /// 距离下一批延迟候选页重新加入活跃队列还需经历的失败次数。
    retry_misses: usize,
}

impl EvictionQueues {
    /// 创建三个队列均为空的回收状态。
    const fn new() -> Self {
        Self {
            active: VecDeque::new(),
            deferred: VecDeque::new(),
            retry_misses: 0,
        }
    }

    /// 将一小批 FIFO 延迟候选页移回 CLOCK 活跃队列。
    ///
    /// 调用者必须已经检查冷却计数，并同时持有页表锁和回收队列锁。
    fn refill_active_from_deferred(&mut self) -> usize {
        let retry_pages = self.deferred.len().min(EVICTION_DEFERRED_RETRY_BATCH);
        for _ in 0..retry_pages {
            let key = self
                .deferred
                .pop_front()
                .expect("deferred eviction queue length changed while locked");
            self.active.push_back(key);
        }
        retry_pages
    }
}

/// 标识请求加载文件页的 VFS 操作来源。
#[derive(Clone, Copy)]
pub enum FilePageCacheSource {
    /// mmap 触发的实际缺页访问。
    MmapDemand,
    /// mmap 路径主动发起的预读。
    MmapPrefetch,
    /// 普通 `read(2)` 请求。
    Read,
    /// `splice(2)` 或相关零拷贝路径。
    Splice,
}

impl FilePage {
    /// 将当前页标记为脏页。
    ///
    /// 调用者在通过共享页帧修改文件内容后，应调用该函数记录缓存页已发生变化。
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// 判断当前页是否为脏页。
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    #[inline]
    /// 设置 CLOCK 访问标记，表示该页最近被使用。
    fn mark_referenced(&self) {
        self.referenced.store(true, Ordering::Relaxed);
    }

    #[inline]
    /// 清除并返回 CLOCK 访问标记。
    fn take_reference(&self) -> bool {
        self.referenced.swap(false, Ordering::AcqRel)
    }
}

/// 基于文件路径和页号组织的全局文件页缓存。
///
/// 缓存内部使用 `BTreeMap` 保存已加载页。缺页通常远少于命中，因此读取路径使用
/// 共享锁；加载和失效才获取独占锁。
/// 该结构只负责缓存页的查找、按需加载和失效，不直接负责脏页回写策略。
pub struct FilePageCache {
    /// 已缓存的文件页集合，先按路径、再按文件内页号索引。
    ///
    /// BuildStorm 的 mmap 和多页 read 会在同一文件中连续探测大量页。把路径
    /// 放在外层能让内层查询只比较页号，而不是在全局页表中反复比较同一段路径。
    pages: RwLock<BTreeMap<Arc<str>, FilePages>>,
    /// 已发布到 `pages` 中的页数。
    ///
    /// 冷页分配或插入前会先预留容量，因此并发发布者不会让缓存超过配置的容量上限。
    page_count: AtomicUsize,
    /// `pages` 中各页的 CLOCK 活跃队列和延迟队列顺序。
    ///
    /// 队列只保存键而不是页的 `Arc`，因此跟踪候选页不会固定其页帧。锁的唯一
    /// 获取顺序是先获取 `pages`、再获取 `eviction_queues`，不存在反向获取路径。
    eviction_queues: Mutex<EvictionQueues>,
}

impl FilePageCache {
    /// 创建一个空的文件页缓存。
    pub const fn new() -> Self {
        Self {
            pages: RwLock::new(BTreeMap::new()),
            page_count: AtomicUsize::new(0),
            eviction_queues: Mutex::new(EvictionQueues::new()),
        }
    }

    /// 返回当前由全局缓存保留的驻留页数量。
    #[inline]
    pub fn cached_page_count(&self) -> usize {
        self.page_count.load(Ordering::Relaxed)
    }

    /// 返回以页为单位表示的全局缓存固定容量。
    #[inline]
    pub const fn max_cached_pages(&self) -> usize {
        MAX_FILE_PAGE_CACHE_PAGES
    }

    /// 在分配冷页前预留一个缓存槽位。
    ///
    /// 如果页帧分配失败，或并发发布者已经赢得相同键，则释放该预留。这样既能
    /// 精确维持全局容量，又无需在页帧分配或文件系统 I/O 期间持有映射锁。
    #[inline]
    fn try_reserve_page(&self) -> bool {
        let mut count = self.page_count.load(Ordering::Relaxed);
        loop {
            if count >= MAX_FILE_PAGE_CACHE_PAGES {
                return false;
            }
            match self.page_count.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(current) => count = current,
            }
        }
    }

    #[inline]
    fn release_page_reservation(&self) {
        self.page_count.fetch_sub(1, Ordering::AcqRel);
    }

    /// 预留一个槽位；缓存已满时，最多回收一个可以安全丢弃的页面后再尝试预留。
    ///
    /// 并发发布者可能抢先使用刚释放的槽位；这种情况下调用者继续沿用原有的
    /// 容量绕过行为。
    #[inline]
    fn reserve_page_or_evict(&self) -> bool {
        self.try_reserve_page() || (self.try_evict_one() && self.try_reserve_page())
    }

    /// 移除一个冷的干净页；该页不能正在由调用者准备，也不能被 VMA 映射。
    ///
    /// `FilePage` 的引用计数保护正在执行的缓存操作；`FrameTracker` 的引用计数
    /// 保护 mmap 和组共享映射，因为这些映射持有的是页帧而不是 `FilePage` 包装器。
    fn try_evict_one(&self) -> bool {
        let mut pages = self.pages.write();
        let mut queues = self.eviction_queues.lock();

        if queues.active.is_empty() && !queues.deferred.is_empty() {
            if queues.retry_misses != 0 {
                queues.retry_misses -= 1;
                #[cfg(feature = "perf")]
                crate::utils::perf::record_file_cache_eviction_cooldown_bypass();
                return false;
            }

            let _retry_pages = queues.refill_active_from_deferred();
            #[cfg(feature = "perf")]
            crate::utils::perf::record_file_cache_eviction_deferred_retry(_retry_pages);
        }

        let scan_budget = queues.active.len().min(EVICTION_SCAN_BUDGET);

        for _ in 0..scan_budget {
            let Some(key) = queues.active.pop_front() else {
                break;
            };
            #[cfg(feature = "perf")]
            crate::utils::perf::record_file_cache_eviction_scan();

            let state = pages
                .get(key.path.as_ref())
                .and_then(|file_pages| file_pages.get(&key.page_index))
                .map(|page| {
                    if page.take_reference() {
                        EvictionState::SecondChance
                    } else if page.is_dirty() {
                        EvictionState::Dirty
                    } else if Arc::strong_count(page) != 1 || Arc::strong_count(&page.frame) != 1 {
                        EvictionState::InUse
                    } else {
                        EvictionState::Evict
                    }
                });

            match state {
                Some(EvictionState::Evict) => {
                    let (removed, file_empty) = {
                        let file_pages = pages
                            .get_mut(key.path.as_ref())
                            .expect("CLOCK candidate disappeared while page cache write lock held");
                        let removed = file_pages.remove(&key.page_index);
                        (removed, file_pages.is_empty())
                    };
                    if file_empty {
                        pages.remove(key.path.as_ref());
                    }
                    debug_assert!(removed.is_some());
                    drop(removed);
                    self.page_count.fetch_sub(1, Ordering::AcqRel);
                    #[cfg(feature = "perf")]
                    crate::utils::perf::record_file_cache_eviction();
                    return true;
                }
                Some(EvictionState::SecondChance) => {
                    #[cfg(feature = "perf")]
                    crate::utils::perf::record_file_cache_eviction_second_chance();
                    queues.active.push_back(key);
                }
                Some(EvictionState::Dirty) => {
                    #[cfg(feature = "perf")]
                    crate::utils::perf::record_file_cache_eviction_dirty_skip();
                    queues.deferred.push_back(key);
                }
                Some(EvictionState::InUse) => {
                    #[cfg(feature = "perf")]
                    crate::utils::perf::record_file_cache_eviction_in_use_skip();
                    queues.deferred.push_back(key);
                }
                None => {
                    // 失效操作会在持有相同锁时移除队列项，因此这里仅用于防御未来的
                    // 缓存管理改动。过期的键不应重新加入队列。
                }
            }
        }

        if queues.active.is_empty() && !queues.deferred.is_empty() {
            queues.retry_misses = EVICTION_DEFERRED_RETRY_MISS_INTERVAL;
        }
        false
    }

    /// 发布调用者已经预留槽位的页面。
    ///
    /// 映射写锁将插入与失效操作串行化；只有映射项创建成功后才加入队列，从而保证
    /// 队列中的每个键都对应一个仍然存在的页面。
    fn publish_reserved_page(&self, page: Arc<FilePage>) -> bool {
        let key = page.key.clone();
        let mut pages = self.pages.write();
        let inserted = match pages
            .entry(key.path.clone())
            .or_default()
            .entry(key.page_index)
        {
            Entry::Vacant(entry) => {
                entry.insert(page);
                true
            }
            Entry::Occupied(_) => false,
        };
        if inserted {
            self.eviction_queues.lock().active.push_back(key);
        }
        inserted
    }

    #[inline]
    /// 记录因缓存容量不足而绕过缓存的页面数量。
    fn record_capacity_bypass(&self, pages: usize) {
        #[cfg(feature = "perf")]
        crate::utils::perf::record_file_cache_capacity_bypass(pages);
        #[cfg(not(feature = "perf"))]
        let _ = pages;
    }

    /// 克隆缓存项并将其标记为最近使用。
    ///
    /// 引用标记会在克隆之后更新：克隆本身已经阻止并发 CLOCK 扫描在调用者拿到
    /// 页面前回收它。
    #[inline]
    fn lookup_page(&self, path: &str, page_index: usize) -> Option<Arc<FilePage>> {
        let page = self
            .pages
            .read()
            .get(path)
            .and_then(|pages| pages.get(&page_index))
            .cloned();
        if let Some(page) = page.as_ref() {
            page.mark_referenced();
        }
        page
    }

    /// 查找指定文件路径和页号对应的缓存页。
    ///
    /// 如果页尚未加载到缓存中，则返回 `None`。
    pub fn get(&self, path: &str, page_index: usize) -> Option<Arc<FilePage>> {
        self.lookup_page(path, page_index)
    }

    /// 使用调用者已经持有的共享路径字符串查找页面。
    ///
    /// 一次 `read(2)` 通常会探测多个页面。使用 `Arc<str>` 保存路径，可以让这些
    /// 查找共享同一份路径字符串，避免每次查找都重新分配和复制。
    pub fn get_shared(&self, path: &Arc<str>, page_index: usize) -> Option<Arc<FilePage>> {
        self.lookup_page(path.as_ref(), page_index)
    }

    /// 通过 inode 稳定的缓存路径查找缓存页。
    ///
    /// 文件映射缺页首先通过 `get_or_load()` 加载页面，然后将页面安装到 VMA 中。
    /// 第二次查找时重新构造 `inode.path()` 会在每次缺页时分配并复制路径。EXT4
    /// inode 专门保留 `Arc<str>` 作为缓存键，其他后端则回退到原始路径字符串。
    pub fn get_inode(&self, inode: &dyn Inode, page_index: usize) -> Option<Arc<FilePage>> {
        if let Some(path) = inode.page_cache_path() {
            self.get_shared(&path, page_index)
        } else {
            let path = inode.path();
            self.get(&path, page_index)
        }
    }

    /// 在范围内所有页面都已缓存时复制该范围的数据。
    ///
    /// 只要有一页未命中，就返回 `None` 且不进入文件系统。调用者随后可以执行一次
    /// 普通读取，并通过 [`insert_read_range`] 发布其中完整覆盖的页面。
    pub fn read_cached_at(&self, path: &str, offset: usize, buf: &mut [u8]) -> Option<usize> {
        if buf.is_empty() {
            return Some(0);
        }

        let path: Arc<str> = Arc::from(path);
        let mut copied = 0;
        while copied < buf.len() {
            let file_offset = offset.checked_add(copied)?;
            let page_index = file_offset / PAGE_SIZE;
            let page_offset = file_offset % PAGE_SIZE;
            let page = self.get_shared(&path, page_index)?;
            if page_offset >= page.valid_len {
                break;
            }
            let count = (page.valid_len - page_offset).min(buf.len() - copied);
            let page_bytes = page.frame.ppn.bytes_array();
            buf[copied..copied + count]
                .copy_from_slice(&page_bytes[page_offset..page_offset + count]);
            copied += count;
            if count == 0 {
                break;
            }
        }

        Some(copied)
    }

    /// 发布普通读取完整覆盖的页面。
    ///
    /// 只有被读取数据完整覆盖的页才会插入缓存，因此部分覆盖的首尾页不会把未初始化
    /// 的数据暴露给后续读取者。
    pub fn insert_read_range(&self, path: &str, offset: usize, data: &[u8], file_size: usize) {
        if data.is_empty() || offset >= file_size {
            return;
        }
        let end = offset.saturating_add(data.len()).min(file_size);
        let path: Arc<str> = Arc::from(path);
        let first_page = offset / PAGE_SIZE;
        let last_page = end.saturating_sub(1) / PAGE_SIZE;
        for page_index in first_page..=last_page {
            let page_start = page_index.saturating_mul(PAGE_SIZE);
            let valid_len = PAGE_SIZE.min(file_size.saturating_sub(page_start));
            if valid_len == 0 || page_start < offset {
                continue;
            }
            let source_start = page_start - offset;
            let Some(source_end) = source_start.checked_add(valid_len) else {
                continue;
            };
            if source_end > data.len() {
                continue;
            }

            let key = FilePageKey {
                path: path.clone(),
                page_index,
            };
            if self
                .pages
                .read()
                .get(path.as_ref())
                .and_then(|pages| pages.get(&page_index))
                .is_some()
            {
                continue;
            }
            if !self.reserve_page_or_evict() {
                self.record_capacity_bypass(1);
                continue;
            }
            let Some(frame) = FrameTracker::alloc() else {
                self.release_page_reservation();
                return;
            };
            frame.ppn.bytes_array_mut()[..valid_len]
                .copy_from_slice(&data[source_start..source_end]);
            let page = Arc::new(FilePage {
                key,
                frame,
                valid_len,
                dirty: AtomicBool::new(false),
                referenced: AtomicBool::new(true),
            });
            if !self.publish_reserved_page(page) {
                self.release_page_reservation();
            }
        }
    }

    /// 获取指定 inode 的文件页，缓存未命中时从底层文件加载。
    ///
    /// 新分配的页帧会先清零，再从 `page_index * PAGE_SIZE` 偏移处读取文件内容。
    /// 如果页起始偏移已经超过文件大小，则返回一个有效长度为 0 的零页。
    ///
    /// 为避免并发加载同一页导致重复插入，函数在完成 I/O 后会再次检查缓存中是否
    /// 已存在同一 `FilePageKey`，若存在则返回已有页。
    ///
    /// 若前一页已经缓存而当前页缺失，访问模式很可能是顺序读取。此时用一次底层
    /// 读取加载一个有上限的相邻页批次，以减少连续 mmap/read/splice 产生的 EXT4
    /// 全局锁获取次数。随机访问、文件尾页和预读页分配失败仍维持单页或较小批次加载。
    pub fn get_or_load(
        &self,
        inode: Arc<dyn Inode>,
        page_index: usize,
        source: FilePageCacheSource,
    ) -> Result<Arc<FilePage>, SysErrNo> {
        #[cfg(not(feature = "perf"))]
        let _ = source;
        #[cfg(feature = "perf")]
        let inode_read_source = match source {
            FilePageCacheSource::MmapDemand => crate::utils::perf::InodeReadSource::MmapDemand,
            FilePageCacheSource::MmapPrefetch => crate::utils::perf::InodeReadSource::MmapPrefetch,
            FilePageCacheSource::Read => crate::utils::perf::InodeReadSource::PageCachedReadColdRun,
            FilePageCacheSource::Splice => crate::utils::perf::InodeReadSource::Other,
        };

        let path = inode
            .page_cache_path()
            .unwrap_or_else(|| Arc::from(inode.path().as_str()));
        let key = FilePageKey {
            path: path.clone(),
            page_index,
        };

        if let Some(page) = self.lookup_page(path.as_ref(), page_index) {
            #[cfg(feature = "perf")]
            {
                crate::utils::perf::record_file_cache_hit();
                match source {
                    FilePageCacheSource::MmapDemand | FilePageCacheSource::MmapPrefetch => {
                        crate::utils::perf::record_file_cache_mmap_hit()
                    }
                    FilePageCacheSource::Read => crate::utils::perf::record_file_cache_read_hit(1),
                    FilePageCacheSource::Splice => {
                        crate::utils::perf::record_file_cache_splice_hit()
                    }
                }
            }
            return Ok(page);
        }

        #[cfg(feature = "perf")]
        {
            crate::utils::perf::record_file_cache_miss();
            match source {
                FilePageCacheSource::MmapDemand | FilePageCacheSource::MmapPrefetch => {
                    crate::utils::perf::record_file_cache_mmap_miss()
                }
                FilePageCacheSource::Read => crate::utils::perf::record_file_cache_read_miss(1),
                FilePageCacheSource::Splice => crate::utils::perf::record_file_cache_splice_miss(),
            }
            crate::utils::perf::record_file_cache_load_attempt();
        }

        let file_size = inode.size();
        let file_offset = page_index * PAGE_SIZE;
        let has_cached_previous = page_index != 0
            && self
                .pages
                .read()
                .get(path.as_ref())
                .is_some_and(|pages| pages.contains_key(&(page_index - 1)));
        let should_readahead = has_cached_previous
            && file_offset.saturating_add(PAGE_SIZE) < file_size
            && page_index.checked_add(1).is_some();
        let wanted_pages = if should_readahead {
            file_size
                .saturating_sub(file_offset)
                .saturating_add(PAGE_SIZE - 1)
                / PAGE_SIZE
        } else {
            1
        }
        .clamp(1, SEQUENTIAL_READAHEAD_MAX_PAGES);

        // 分配失败只会缩短推测性预读尾部；触发缺页的首个页面仍必须可用，否则应向
        // 调用者报告真正的 ENOMEM。
        let mut frames = Vec::with_capacity(wanted_pages);
        frames.push(FrameTracker::alloc().ok_or(SysErrNo::ENOMEM)?);
        while frames.len() < wanted_pages {
            let Some(frame) = FrameTracker::alloc() else {
                break;
            };
            frames.push(frame);
        }

        let read_len = if file_offset >= file_size {
            0
        } else if frames.len() == 1 {
            inode.read_at(file_offset, frames[0].ppn.bytes_array_mut())?
        } else {
            let mut read_buf = alloc::vec![0; frames.len() * PAGE_SIZE];
            let read_len = inode.read_at(file_offset, &mut read_buf)?;
            for (index, frame) in frames.iter().enumerate() {
                let page_start = index * PAGE_SIZE;
                let valid_len = read_len.saturating_sub(page_start).min(PAGE_SIZE);
                if valid_len == 0 {
                    break;
                }
                frame.ppn.bytes_array_mut()[..valid_len]
                    .copy_from_slice(&read_buf[page_start..page_start + valid_len]);
            }
            read_len
        };
        #[cfg(feature = "perf")]
        if file_offset < file_size {
            crate::utils::perf::record_inode_read_source(inode_read_source, read_len);
        }

        let mut loaded_pages = Vec::with_capacity(frames.len());
        for (index, frame) in frames.into_iter().enumerate() {
            let page_start = index * PAGE_SIZE;
            let valid_len = read_len.saturating_sub(page_start).min(PAGE_SIZE);
            // 第一个页面代表实际缺页，包括文件尾的零长度哨兵页。尾部空的推测性
            // 预读页不发布到缓存。
            if index != 0 && valid_len == 0 {
                break;
            }
            let Some(loaded_page_index) = page_index.checked_add(index) else {
                break;
            };
            loaded_pages.push(Arc::new(FilePage {
                key: FilePageKey {
                    path: key.path.clone(),
                    page_index: loaded_page_index,
                },
                frame,
                valid_len,
                dirty: AtomicBool::new(false),
                referenced: AtomicBool::new(true),
            }));
        }
        let page = loaded_pages.remove(0);

        if let Some(existing) = self.lookup_page(path.as_ref(), page_index) {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_file_cache_load_race();
            return Ok(existing);
        }
        if !self.reserve_page_or_evict() {
            self.record_capacity_bypass(1 + loaded_pages.len());
            return Ok(page);
        }
        if !self.publish_reserved_page(page.clone()) {
            self.release_page_reservation();
            #[cfg(feature = "perf")]
            crate::utils::perf::record_file_cache_load_race();
            if let Some(existing) = self.lookup_page(path.as_ref(), page_index) {
                return Ok(existing);
            }
            return Ok(page);
        }

        let mut readahead_pages = 0;
        let mut readahead_bytes = 0;
        let mut readahead = loaded_pages.into_iter();
        while let Some(readahead_page) = readahead.next() {
            let readahead_page_index = readahead_page.key.page_index;
            if self
                .pages
                .read()
                .get(path.as_ref())
                .is_some_and(|file_pages| file_pages.contains_key(&readahead_page_index))
            {
                continue;
            }
            if !self.reserve_page_or_evict() {
                self.record_capacity_bypass(1 + readahead.len());
                break;
            }
            let readahead_len = readahead_page.valid_len;
            if self.publish_reserved_page(readahead_page) {
                readahead_pages += 1;
                readahead_bytes += readahead_len;
            } else {
                self.release_page_reservation();
            }
        }
        #[cfg(not(feature = "perf"))]
        let _ = (readahead_pages, readahead_bytes);
        #[cfg(feature = "perf")]
        if readahead_pages != 0 {
            crate::utils::perf::record_file_cache_readahead(readahead_pages, readahead_bytes);
        }
        Ok(page)
    }

    /// 失效指定文件路径在字节范围 `[start, start + len)` 内覆盖的缓存页。
    ///
    /// `len` 为 0 时不做任何处理。范围端点会按页大小向外扩展，确保所有与该字节
    /// 范围相交的页面都会从缓存中移除。失效不会负责把脏页写回底层文件。
    pub fn invalidate_path_range(&self, path: &str, start: usize, len: usize) {
        if len == 0 {
            return;
        }
        let first = start / PAGE_SIZE;
        let end = start.saturating_add(len);
        let last = end.saturating_add(PAGE_SIZE - 1) / PAGE_SIZE;
        let path: Arc<str> = Arc::from(path);
        let mut pages = self.pages.write();
        let removed = if let Some(file_pages) = pages.get_mut(path.as_ref()) {
            let mut removed = 0;
            for page_index in first..last {
                if file_pages.remove(&page_index).is_some() {
                    removed += 1;
                }
            }
            (removed, file_pages.is_empty())
        } else {
            (0, false)
        };
        if removed.1 {
            pages.remove(path.as_ref());
        }
        if removed.0 != 0 {
            self.page_count.fetch_sub(removed.0, Ordering::AcqRel);
            let mut queues = self.eviction_queues.lock();
            let retain = |key: &FilePageKey| {
                key.path.as_ref() != path.as_ref()
                    || key.page_index < first
                    || key.page_index >= last
            };
            queues.active.retain(retain);
            queues.deferred.retain(retain);
        }
    }

    /// 失效指定文件路径的全部缓存页。
    ///
    /// 该操作同时从 CLOCK 活跃队列和延迟队列删除路径对应的候选项，并更新全局
    /// 驻留页计数。调用者应确保底层文件的最新内容已经可供后续缺页读取。
    pub fn invalidate_path(&self, path: &str) {
        let mut pages = self.pages.write();
        if let Some(file_pages) = pages.remove(path) {
            self.page_count
                .fetch_sub(file_pages.len(), Ordering::AcqRel);
            let mut queues = self.eviction_queues.lock();
            let retain = |key: &FilePageKey| key.path.as_ref() != path;
            queues.active.retain(retain);
            queues.deferred.retain(retain);
        }
    }
}

/// 系统范围内共享的文件页缓存实例。
pub static FILE_PAGE_CACHE: FilePageCache = FilePageCache::new();
