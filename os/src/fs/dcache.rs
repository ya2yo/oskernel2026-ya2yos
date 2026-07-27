//! VFS dentry cache。
//!
//! 这一层缓存的是“已缓存父目录 inode + 子项名称”到子 inode 的映射，用来避免
//! `open()` 等热路径在父目录已经命中 `FsIndex` 后，仍反复进入底层 ext4 路径查找。
//! cache 只维护 VFS 层的目录项关系；真正的目录项增删仍由文件系统操作完成。

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::mem;
use hashbrown::HashMap;
use spin::{Lazy, RwLock};

use super::Inode;

/// 目录项缓存键。
///
/// `parent` 使用父目录 `Arc<dyn Inode>` 的对象地址作为身份，避免为了构造 cache key
/// 再调用 `path()` 或 `fstat()`，否则缓存命中本身也会触发额外元数据查询。
/// `name` 是父目录下的单个子项名，不是完整路径。
#[derive(Clone, Eq, Hash, PartialEq)]
struct DentryKey {
    parent: usize,
    name: String,
}

/// 单个 dentry cache 项。
///
/// Positive 表示子项存在，并直接保存子 inode；Negative 表示已经确认子项不存在。
/// Negative cache 主要服务非 `O_CREAT` 查找路径，创建路径应绕过或失效旧的 negative 项。
enum DentryValue {
    /// Keep the child alive across close/open cycles in the same process.
    /// Cache-pressure eviction drops these strong references before reclaiming
    /// otherwise-unused FsIndex entries.
    Positive {
        inode: Arc<dyn Inode>,
    },
    Negative,
}

/// 对外暴露的 dentry 查找结果。
///
/// 调用者只需要区分“命中存在的 inode”和“命中不存在”，不需要知道内部缓存项格式。
pub enum DentryLookup {
    Positive(Arc<dyn Inode>),
    Negative,
}

/// 全局 VFS dentry cache。
///
/// 当前内核采用有界的 `HashMap + RwLock`：create/link/unlink/symlink/rename 路径显式
/// 回填或失效条目，缓存达到上限和 process/testcase 回收边界时统一清理。
pub struct DentryCache {
    entries: RwLock<HashMap<DentryKey, DentryValue>>,
}

/// Bound path-name metadata even while one long-running process continually
/// probes unique names.  Cargo's parallel compiler workers share this global
/// cache, so keep one cold BuildStorm build resident instead of repeatedly
/// flushing the working set at a few thousand paths.
const MAX_DENTRY_CACHE_ENTRIES: usize = 32 * 1024;

impl DentryCache {
    /// 创建空的 dentry cache。
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// 查找父目录下的子项缓存。
    ///
    /// 返回 `None` 表示 cache miss，调用者需要继续走底层文件系统查找；
    /// 返回 `Positive` 可直接复用 inode，返回 `Negative` 可直接按不存在处理。
    pub fn lookup(&self, parent: &Arc<dyn Inode>, name: &str) -> Option<DentryLookup> {
        let key = Self::key(parent, name);
        let entries = self.entries.read();
        let lookup = match entries.get(&key) {
            Some(DentryValue::Positive { inode }) => Some(DentryLookup::Positive(inode.clone())),
            // Negative cache 让重复的“确认不存在”查询不用再次进入 ext4。
            Some(DentryValue::Negative) => Some(DentryLookup::Negative),
            None => None,
        };
        #[cfg(feature = "perf")]
        match &lookup {
            Some(DentryLookup::Positive(_)) => crate::utils::perf::record_vfs_dentry_positive_hit(),
            Some(DentryLookup::Negative) => crate::utils::perf::record_vfs_dentry_negative_hit(),
            None => crate::utils::perf::record_vfs_dentry_miss(),
        }
        lookup
    }

    /// 插入存在的子项缓存。
    ///
    /// 通常在 `open()` 查找成功、`create_file()` 创建成功、`linkat()` 物化成功后调用。
    pub fn insert_positive(&self, parent: &Arc<dyn Inode>, name: &str, inode: Arc<dyn Inode>) {
        self.insert(Self::key(parent, name), DentryValue::Positive { inode });
    }

    /// 插入不存在的子项缓存。
    ///
    /// 只适合非创建路径的 `ENOENT` 结果；创建、link、rename 等可能改变目录项的操作
    /// 必须先失效或覆盖对应缓存。
    pub fn insert_negative(&self, parent: &Arc<dyn Inode>, name: &str) {
        self.insert(Self::key(parent, name), DentryValue::Negative);
    }

    /// 失效父目录下的单个子项缓存。
    ///
    /// 用于 unlink、rename、symlink、link 或 create 前后，确保后续查找不会命中旧结果。
    pub fn invalidate(&self, parent: &Arc<dyn Inode>, name: &str) {
        let key = Self::key(parent, name);
        let removed = { self.entries.write().remove(&key) };
        drop(removed);
    }

    /// 失效某个父目录下的所有子项缓存。
    ///
    /// 当无法精确知道哪个 child 发生变化，或父目录整体状态发生变化时使用。
    pub fn invalidate_parent(&self, parent: &Arc<dyn Inode>) {
        let parent = Self::parent_key(parent);
        let capacity = self.entries.read().len();
        let mut removed_inodes = Vec::with_capacity(capacity);
        // 只删除指定父目录的目录项，避免一次目录变化冲掉整个全局 cache。
        {
            let mut entries = self.entries.write();
            entries.retain(|key, value| {
                if key.parent != parent {
                    return true;
                }
                if let DentryValue::Positive { inode } = value {
                    // Keep the inode alive until the dcache lock is released:
                    // Ext4Inode::drop() takes EXT4_OP_LOCK.
                    removed_inodes.push(inode.clone());
                }
                false
            });
        }
        drop(removed_inodes);
    }

    /// Drop all cached path-name metadata at a process boundary.  This is safe
    /// because both positive and negative entries are accelerators only.
    pub fn clear(&self) -> usize {
        let removed = {
            let mut entries = self.entries.write();
            mem::take(&mut *entries)
        };
        let count = removed.len();
        drop(removed);
        count
    }

    fn insert(&self, key: DentryKey, value: DentryValue) {
        let (evicted, replaced) = {
            let mut entries = self.entries.write();
            let evicted =
                if !entries.contains_key(&key) && entries.len() >= MAX_DENTRY_CACHE_ENTRIES {
                    Some(mem::take(&mut *entries))
                } else {
                    None
                };
            let replaced = entries.insert(key, value);
            (evicted, replaced)
        };
        #[cfg(feature = "perf")]
        if let Some(entries) = evicted.as_ref() {
            crate::utils::perf::record_vfs_dentry_capacity_evict(entries.len());
        }
        drop(replaced);
        drop(evicted);
    }

    /// 构造 `(parent inode identity, child name)` 形式的 dentry key。
    fn key(parent: &Arc<dyn Inode>, name: &str) -> DentryKey {
        DentryKey {
            parent: Self::parent_key(parent),
            name: name.to_string(),
        }
    }

    /// 获取父 inode 的 VFS 对象身份。
    ///
    /// 这里使用 trait object 的 data pointer 地址作为 key，要求同一个底层 inode 先经过
    /// `FsIndex` 归一化；否则不同 `Arc` 对象会被视为不同父目录。
    fn parent_key(parent: &Arc<dyn Inode>) -> usize {
        Arc::as_ptr(parent) as *const () as usize
    }
}

/// 全局 dentry cache 实例。
pub static DENTRY_CACHE: Lazy<DentryCache> = Lazy::new(DentryCache::new);
