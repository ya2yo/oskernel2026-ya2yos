use alloc::{collections::BTreeMap, string::String, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::{arch::memory_layout::PAGE_SIZE, fs::Inode, mm::FrameTracker, utils::SysErrNo};

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct FilePageKey {
    pub path: String,
    pub page_index: usize,
}

pub struct FilePage {
    pub key: FilePageKey,
    pub frame: Arc<FrameTracker>,
    pub valid_len: usize,
    dirty: AtomicBool,
}

impl FilePage {
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }
}

pub struct FilePageCache {
    pages: Mutex<BTreeMap<FilePageKey, Arc<FilePage>>>,
}

impl FilePageCache {
    pub const fn new() -> Self {
        Self {
            pages: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn get(&self, path: &str, page_index: usize) -> Option<Arc<FilePage>> {
        self.pages
            .lock()
            .get(&FilePageKey {
                path: String::from(path),
                page_index,
            })
            .cloned()
    }

    pub fn get_or_load(
        &self,
        inode: Arc<dyn Inode>,
        page_index: usize,
    ) -> Result<Arc<FilePage>, SysErrNo> {
        let path = inode.path();
        let key = FilePageKey { path, page_index };

        if let Some(page) = self.pages.lock().get(&key).cloned() {
            return Ok(page);
        }

        let frame = FrameTracker::alloc().ok_or(SysErrNo::ENOMEM)?;
        let bytes = frame.ppn.bytes_array_mut();
        bytes.fill(0);

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

    pub fn invalidate_path(&self, path: &str) {
        self.pages.lock().retain(|key, _| key.path != path);
    }
}

pub static FILE_PAGE_CACHE: FilePageCache = FilePageCache::new();
