use alloc::{collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::RwLock;

use crate::{arch::memory_layout::PAGE_SIZE, fs::Inode, mm::FrameTracker, utils::SysErrNo};

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
}

impl FilePageCache {
    /// 创建一个空的文件页缓存。
    pub const fn new() -> Self {
        Self {
            pages: RwLock::new(BTreeMap::new()),
        }
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
            let Some(frame) = FrameTracker::alloc() else {
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
            pages
                .entry(path.clone())
                .or_default()
                .entry(page_index)
                .or_insert(page);
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
    /// 若前一页已经缓存而当前页缺失，访问模式很可能是顺序读取。此时在一次底层
    /// 读取中同时加载当前页和下一页，以减少连续 mmap/read/splice 产生的 EXT4
    /// 全局锁获取次数。随机访问、文件尾页和预读页分配失败仍维持单页加载。
    pub fn get_or_load(
        &self,
        inode: Arc<dyn Inode>,
        page_index: usize,
    ) -> Result<Arc<FilePage>, SysErrNo> {
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
            crate::utils::perf::record_file_cache_hit();
            return Ok(page);
        }

        #[cfg(feature = "perf")]
        crate::utils::perf::record_file_cache_miss();

        let file_size = inode.size();
        let file_offset = page_index * PAGE_SIZE;
        let next_key = page_index
            .checked_add(1)
            .map(|next_page_index| FilePageKey {
                path: key.path.clone(),
                page_index: next_page_index,
            });
        let has_cached_previous = page_index != 0
            && self
                .pages
                .read()
                .get(path.as_ref())
                .is_some_and(|pages| pages.contains_key(&(page_index - 1)));
        let should_readahead = has_cached_previous
            && file_offset.saturating_add(PAGE_SIZE) < file_size
            && next_key.is_some();

        let frame = FrameTracker::alloc().ok_or(SysErrNo::ENOMEM)?;
        let next_frame = if should_readahead {
            FrameTracker::alloc()
        } else {
            None
        };

        let (valid_len, readahead_page) =
            if let (Some(next_key), Some(next_frame)) = (next_key.as_ref(), next_frame) {
                let mut read_buf = alloc::vec![0; PAGE_SIZE * 2];
                let read_len = if file_offset < file_size {
                    inode.read_at(file_offset, &mut read_buf)?
                } else {
                    0
                };
                let valid_len = read_len.min(PAGE_SIZE);
                frame.ppn.bytes_array_mut()[..valid_len].copy_from_slice(&read_buf[..valid_len]);

                let readahead_len = read_len.saturating_sub(PAGE_SIZE).min(PAGE_SIZE);
                let readahead_page = if readahead_len == 0 {
                    None
                } else {
                    next_frame.ppn.bytes_array_mut()[..readahead_len]
                        .copy_from_slice(&read_buf[PAGE_SIZE..PAGE_SIZE + readahead_len]);
                    Some(Arc::new(FilePage {
                        key: next_key.clone(),
                        frame: next_frame,
                        valid_len: readahead_len,
                        dirty: AtomicBool::new(false),
                    }))
                };
                (valid_len, readahead_page)
            } else {
                let bytes = frame.ppn.bytes_array_mut();
                let valid_len = if file_offset < file_size {
                    inode.read_at(file_offset, bytes)?
                } else {
                    0
                };
                (valid_len, None)
            };

        let page = Arc::new(FilePage {
            key,
            frame,
            valid_len,
            dirty: AtomicBool::new(false),
        });

        let mut pages = self.pages.write();
        let file_pages = pages.entry(path.clone()).or_default();
        if let Some(existing) = file_pages.get(&page_index).cloned() {
            return Ok(existing);
        }
        file_pages.insert(page_index, page.clone());
        let readahead_bytes = readahead_page.and_then(|readahead_page| {
            let readahead_bytes = readahead_page.valid_len;
            let readahead_page_index = readahead_page.key.page_index;
            if file_pages.contains_key(&readahead_page_index) {
                None
            } else {
                file_pages.insert(readahead_page_index, readahead_page);
                Some(readahead_bytes)
            }
        });
        drop(pages);
        #[cfg(not(feature = "perf"))]
        let _ = readahead_bytes;
        #[cfg(feature = "perf")]
        if let Some(readahead_bytes) = readahead_bytes {
            crate::utils::perf::record_file_cache_readahead(1, readahead_bytes);
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
        let remove_path = if let Some(file_pages) = pages.get_mut(path.as_ref()) {
            for page_index in first..last {
                file_pages.remove(&page_index);
            }
            file_pages.is_empty()
        } else {
            false
        };
        if remove_path {
            pages.remove(path.as_ref());
        }
    }

    /// 失效指定文件路径的全部缓存页。
    pub fn invalidate_path(&self, path: &str) {
        self.pages.write().remove(path);
    }
}

/// 系统范围内共享的文件页缓存实例。
pub static FILE_PAGE_CACHE: FilePageCache = FilePageCache::new();
