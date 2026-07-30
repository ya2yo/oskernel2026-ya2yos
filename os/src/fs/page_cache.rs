use alloc::{collections::btree_map::Entry, collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::RwLock;

use crate::{arch::memory_layout::PAGE_SIZE, fs::Inode, mm::FrameTracker, utils::SysErrNo};

/// Maximum number of adjacent pages a confirmed sequential fault may load in
/// one filesystem read.  This deliberately stays small: the global cache has
/// a fixed capacity but no eviction policy, while 16 KiB is enough to amortize
/// the lwext4 gate over common ELF and source-file mmap walks.
const SEQUENTIAL_READAHEAD_MAX_PAGES: usize = 4;
/// Keep the global page cache bounded even when one long-lived compiler walks
/// large source and artifact files.  This is 384 MiB with the current 4 KiB
/// page size, leaving room for the mmap working set while permitting normal
/// read caching above the former 8 MiB per-file threshold.
const MAX_FILE_PAGE_CACHE_PAGES: usize = 96 * 1024;

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
}

/// Pages belonging to one pathname.  Grouping the page number beneath the
/// pathname avoids comparing that pathname repeatedly while a read or mmap
/// walk probes adjacent pages from the same file.
type FilePages = BTreeMap<usize, Arc<FilePage>>;

/// Identifies the VFS path that requested a page-cache load.
#[derive(Clone, Copy)]
pub enum FilePageCacheSource {
    Mmap,
    Read,
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
    /// Number of pages published in `pages`. Reservations are taken before a
    /// cold page is allocated or inserted, so concurrent publishers cannot
    /// push the cache beyond its configured capacity.
    page_count: AtomicUsize,
}

impl FilePageCache {
    /// 创建一个空的文件页缓存。
    pub const fn new() -> Self {
        Self {
            pages: RwLock::new(BTreeMap::new()),
            page_count: AtomicUsize::new(0),
        }
    }

    /// Number of resident pages currently retained by the global cache.
    #[inline]
    pub fn cached_page_count(&self) -> usize {
        self.page_count.load(Ordering::Relaxed)
    }

    /// Fixed global cache capacity, expressed in pages.
    #[inline]
    pub const fn max_cached_pages(&self) -> usize {
        MAX_FILE_PAGE_CACHE_PAGES
    }

    /// Reserve one cache slot before a cold page is allocated. The reservation
    /// is released when allocation fails or a concurrent publisher won the
    /// same key, keeping the global cap exact without holding the map lock
    /// across allocation or filesystem I/O.
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

    #[inline]
    fn record_capacity_bypass(&self, pages: usize) {
        #[cfg(feature = "perf")]
        crate::utils::perf::record_file_cache_capacity_bypass(pages);
        #[cfg(not(feature = "perf"))]
        let _ = pages;
    }

    /// 查找指定文件路径和页号对应的缓存页。
    ///
    /// 如果页尚未加载到缓存中，则返回 `None`。
    pub fn get(&self, path: &str, page_index: usize) -> Option<Arc<FilePage>> {
        self.pages
            .read()
            .get(path)
            .and_then(|pages| pages.get(&page_index))
            .cloned()
    }

    /// Look up a page with a path allocation shared by the caller.
    ///
    /// A single `read(2)` commonly probes several pages.  Keeping the path in
    /// an `Arc<str>` lets those probes borrow the same pathname without
    /// allocating and copying it for every lookup.
    pub fn get_shared(&self, path: &Arc<str>, page_index: usize) -> Option<Arc<FilePage>> {
        self.pages
            .read()
            .get(path.as_ref())
            .and_then(|pages| pages.get(&page_index))
            .cloned()
    }

    /// Look up a cached page through the inode's stable cache pathname.
    ///
    /// File-backed page faults first load a page through `get_or_load()` and
    /// then install that page in a VMA.  Rebuilding `inode.path()` for the
    /// second lookup allocates and copies a pathname on every fault.  EXT4
    /// inodes retain an `Arc<str>` specifically for this cache key, while
    /// other backends keep the original path-string fallback.
    pub fn get_inode(&self, inode: &dyn Inode, page_index: usize) -> Option<Arc<FilePage>> {
        if let Some(path) = inode.page_cache_path() {
            self.get_shared(&path, page_index)
        } else {
            let path = inode.path();
            self.get(&path, page_index)
        }
    }

    /// Copy a range when every page is already cached.
    ///
    /// A miss returns `None` without entering the filesystem.  Callers can
    /// then perform one normal read and publish the returned full pages with
    /// [`insert_read_range`].
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

    /// Publish complete pages covered by a normal read.
    ///
    /// Only bytes wholly covered by the read are inserted, so a partial first
    /// or last page can never expose uninitialised data to a later reader.
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
            if !self.try_reserve_page() {
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
            });
            let mut pages = self.pages.write();
            let inserted = match pages.entry(path.clone()).or_default().entry(page_index) {
                Entry::Vacant(entry) => {
                    entry.insert(page);
                    true
                }
                Entry::Occupied(_) => false,
            };
            if !inserted {
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
            FilePageCacheSource::Mmap => crate::utils::perf::InodeReadSource::MmapCacheFill,
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

        if let Some(page) = self
            .pages
            .read()
            .get(path.as_ref())
            .and_then(|pages| pages.get(&page_index))
            .cloned()
        {
            #[cfg(feature = "perf")]
            {
                crate::utils::perf::record_file_cache_hit();
                match source {
                    FilePageCacheSource::Mmap => crate::utils::perf::record_file_cache_mmap_hit(),
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
                FilePageCacheSource::Mmap => crate::utils::perf::record_file_cache_mmap_miss(),
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

        // Allocation failure only shortens the speculative tail.  The faulting
        // page must still be available or this is a real ENOMEM for the caller.
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
            // The first page represents the fault itself, including an EOF
            // sentinel.  Empty trailing speculative pages are not published.
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
            }));
        }
        let page = loaded_pages.remove(0);

        let mut pages = self.pages.write();
        if let Some(existing) = pages
            .get(path.as_ref())
            .and_then(|file_pages| file_pages.get(&page_index))
            .cloned()
        {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_file_cache_load_race();
            return Ok(existing);
        }
        if !self.try_reserve_page() {
            self.record_capacity_bypass(1 + loaded_pages.len());
            return Ok(page);
        }
        let file_pages = pages.entry(path.clone()).or_default();
        file_pages.insert(page_index, page.clone());
        let mut readahead_pages = 0;
        let mut readahead_bytes = 0;
        let mut readahead = loaded_pages.into_iter();
        while let Some(readahead_page) = readahead.next() {
            let readahead_page_index = readahead_page.key.page_index;
            if !file_pages.contains_key(&readahead_page_index) {
                if !self.try_reserve_page() {
                    self.record_capacity_bypass(1 + readahead.len());
                    break;
                }
                readahead_pages += 1;
                readahead_bytes += readahead_page.valid_len;
                file_pages.insert(readahead_page_index, readahead_page);
            }
        }
        drop(pages);
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
    /// 范围相交的页都会从缓存中移除。
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
        }
    }

    /// 失效指定文件路径的全部缓存页。
    pub fn invalidate_path(&self, path: &str) {
        if let Some(file_pages) = self.pages.write().remove(path) {
            self.page_count
                .fetch_sub(file_pages.len(), Ordering::AcqRel);
        }
    }
}

/// 系统范围内共享的文件页缓存实例。
pub static FILE_PAGE_CACHE: FilePageCache = FilePageCache::new();
