use alloc::{collections::BTreeMap, string::String, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::{arch::memory_layout::PAGE_SIZE, fs::Inode, mm::FrameTracker, utils::SysErrNo};

/// 文件页缓存的索引键。
///
/// 同一个文件由路径唯一标识，文件内容按 `PAGE_SIZE` 切分后使用
/// `page_index` 定位具体页。
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct FilePageKey {
    /// 被缓存文件的路径。
    pub path: String,
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
/// 缓存内部使用 `BTreeMap` 保存已加载页，并由 `Mutex` 保护并发访问。
/// 该结构只负责缓存页的查找、按需加载和失效，不直接负责脏页回写策略。
pub struct FilePageCache {
    /// 已缓存的文件页集合。
    pages: Mutex<BTreeMap<FilePageKey, Arc<FilePage>>>,
}

impl FilePageCache {
    /// 创建一个空的文件页缓存。
    pub const fn new() -> Self {
        Self {
            pages: Mutex::new(BTreeMap::new()),
        }
    }

    /// 查找指定文件路径和页号对应的缓存页。
    ///
    /// 如果页尚未加载到缓存中，则返回 `None`。
    pub fn get(&self, path: &str, page_index: usize) -> Option<Arc<FilePage>> {
        self.pages
            .lock()
            .get(&FilePageKey {
                path: String::from(path),
                page_index,
            })
            .cloned()
    }

    /// 获取指定 inode 的文件页，缓存未命中时从底层文件加载。
    ///
    /// 新分配的页帧会先清零，再从 `page_index * PAGE_SIZE` 偏移处读取文件内容。
    /// 如果页起始偏移已经超过文件大小，则返回一个有效长度为 0 的零页。
    ///
    /// 为避免并发加载同一页导致重复插入，函数在完成 I/O 后会再次检查缓存中是否
    /// 已存在同一 `FilePageKey`，若存在则返回已有页。
    pub fn get_or_load(
        &self,
        inode: Arc<dyn Inode>,
        page_index: usize,
    ) -> Result<Arc<FilePage>, SysErrNo> {
        let path = inode.path();
        let key = FilePageKey { path, page_index };

        if let Some(page) = self.pages.lock().get(&key).cloned() {
            // crate::perf::record_file_cache_hit();
            return Ok(page);
        }

        // crate::perf::record_file_cache_miss();

        let frame = FrameTracker::alloc().ok_or(SysErrNo::ENOMEM)?;
        let bytes = frame.ppn.bytes_array_mut();

        let file_offset = page_index * PAGE_SIZE;
        let valid_len = if file_offset < inode.size() {
            inode.read_at(file_offset, bytes)?
        } else {
            0
        };

        let page = Arc::new(FilePage {
            key: key.clone(),
            frame,
            valid_len,
            dirty: AtomicBool::new(false),
        });

        let mut pages = self.pages.lock();
        if let Some(existing) = pages.get(&key).cloned() {
            return Ok(existing);
        }
        pages.insert(key, page.clone());
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
        let mut pages = self.pages.lock();
        for page_index in first..last {
            pages.remove(&FilePageKey {
                path: String::from(path),
                page_index,
            });
        }
    }

    /// 失效指定文件路径的全部缓存页。
    pub fn invalidate_path(&self, path: &str) {
        self.pages.lock().retain(|key, _| key.path != path);
    }
}

/// 系统范围内共享的文件页缓存实例。
pub static FILE_PAGE_CACHE: FilePageCache = FilePageCache::new();
