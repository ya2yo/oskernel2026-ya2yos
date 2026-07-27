use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use hashbrown::HashMap;
use spin::{Lazy, RwLock};

use super::{Inode, InodeType};

/// VFS inode cache 的身份键。
///
/// 正常文件使用 Linux 可见的 `(st_dev, st_ino)` 标识 inode 身份；如果底层
/// 文件系统暂时无法提供有效 inode 号，则退回到路径键，保持旧路径缓存语义。
#[derive(Clone, Eq, PartialEq, Hash)]
enum InodeCacheKey {
    /// 有效 inode 号时的身份键。概念上对应 Linux inode cache 中的
    /// `(super_block, i_ino)`；这里以用户可见的 `(st_dev, st_ino)` 表示。
    Inode { dev: usize, ino: usize },
    /// 不提供 inode 号的底层文件系统的兼容性回退键。
    Path(String),
}

/// 路径索引与 inode 对象缓存必须在一次锁持有中更新。
///
/// 如果先更新路径索引、后更新 inode 缓存（或相反），底层 ext4 在两步之间复用
/// inode 号时，新文件可能会错误拿到已删除文件的 `Arc<dyn Inode>`。
#[derive(Default)]
struct InodeCacheState {
    /// `绝对路径 -> inode 身份键`。硬链接可对应多个路径但共享同一个键。
    paths: HashMap<String, InodeCacheKey>,
    /// `inode 身份键 -> 规范的 inode 对象`。
    inodes: HashMap<InodeCacheKey, Arc<dyn Inode>>,
}

static INODE_CACHE: Lazy<RwLock<InodeCacheState>> =
    Lazy::new(|| RwLock::new(InodeCacheState::default()));

/// 特殊节点类型补充表：`绝对路径 -> InodeType`。
///
/// 用于 FIFO、设备节点和路径名 UNIX socket 等节点，在底层 inode 无法稳定提供
/// 类型时保留其创建时的类型。Linux 不使用独立表，而是将此信息编码在
/// `struct inode::i_mode` 的 `S_IF*` 类型位中。
static SPECIAL_NODE_TYPES: Lazy<RwLock<HashMap<String, InodeType>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// A lookup cache must not grow without bound.  BuildStorm opens substantially
/// more than 4096 distinct source and artifact paths in one cold build; keep a
/// full working set here and reclaim it only under the explicit cache budget.
const MAX_CACHED_INODES: usize = 32 * 1024;

pub struct FsIndex;

impl FsIndex {
    pub fn has_inode(path: &str) -> bool {
        Self::find_inode_idx(path).is_some()
    }

    pub fn find_inode_idx(path: &str) -> Option<Arc<dyn Inode>> {
        let cache = INODE_CACHE.read();
        let inode = cache
            .paths
            .get(path)
            .and_then(|key| cache.inodes.get(key))
            .cloned();
        #[cfg(feature = "perf")]
        if inode.is_some() {
            crate::utils::perf::record_vfs_fsidx_hit();
        } else {
            crate::utils::perf::record_vfs_fsidx_miss();
        }
        inode
    }

    pub fn insert_inode_idx(path: &str, inode: Arc<dyn Inode>) -> Arc<dyn Inode> {
        Self::reclaim_if_at_capacity();
        let key = Self::cache_key(path, &inode);
        let canonical = loop {
            let existing = {
                let cache = INODE_CACHE.read();
                cache.inodes.get(&key).cloned()
            };

            let Some(existing) = existing else {
                let (canonical, replaced, displaced) = {
                    let mut cache = INODE_CACHE.write();
                    if cache.inodes.contains_key(&key) {
                        continue;
                    }
                    let replaced = cache.inodes.insert(key.clone(), inode.clone());
                    let displaced = Self::bind_path(&mut cache, path, &key);
                    (inode.clone(), replaced, displaced)
                };
                // Removing a cache entry may be the last Arc and invoke
                // Ext4Inode::drop(), which takes EXT4_OP_LOCK.  Never do that
                // while holding the index lock.
                drop(replaced);
                drop(displaced);
                break canonical;
            };

            let replace_stale = !Self::inode_matches_key(&existing, &key);
            let (canonical, replaced, displaced, record_alias) = {
                let mut cache = INODE_CACHE.write();
                let Some(current) = cache.inodes.get(&key).cloned() else {
                    continue;
                };
                if !Arc::ptr_eq(&current, &existing) {
                    continue;
                }

                if replace_stale {
                    // Every path under this key refers to an inode that has just
                    // been proven dead.  Keeping any of those aliases would make
                    // a later lookup route the old pathname to the replacement.
                    cache.paths.retain(|_, candidate| candidate != &key);
                    let replaced = cache.inodes.insert(key.clone(), inode.clone());
                    let displaced = Self::bind_path(&mut cache, path, &key);
                    // `inode` was constructed from `path`, so its initial
                    // alias list already contains this path.  Recording it
                    // again would needlessly enter EXT4_OP_LOCK.
                    (inode.clone(), replaced, displaced, false)
                } else {
                    // A path already bound to this canonical inode has
                    // already been recorded in Ext4Inode's alias list.  Only
                    // a genuinely new path (for example, a hard link) needs
                    // the lock-protected alias update below.
                    let path_already_bound = cache
                        .paths
                        .get(path)
                        .map_or(false, |candidate| candidate == &key);
                    let displaced = Self::bind_path(&mut cache, path, &key);
                    (current, None, displaced, !path_already_bound)
                }
            };
            drop(replaced);
            drop(displaced);
            if record_alias {
                canonical.cache_path_alias(path);
            }
            break canonical;
        };
        canonical
    }

    pub fn insert_special_node_type(path: &str, inode_type: InodeType) {
        SPECIAL_NODE_TYPES
            .write()
            .insert(path.to_string(), inode_type);
    }

    pub fn special_node_type(path: &str) -> Option<InodeType> {
        SPECIAL_NODE_TYPES.read().get(path).copied()
    }

    pub fn remove_inode_idx(path: &str) {
        let removed_inode = {
            let mut cache = INODE_CACHE.write();
            if let Some(key) = cache.paths.remove(path) {
                if !cache.paths.values().any(|candidate| candidate == &key) {
                    cache.inodes.remove(&key)
                } else {
                    None
                }
            } else {
                None
            }
        };
        drop(removed_inode);
        SPECIAL_NODE_TYPES.write().remove(path);
    }

    /// Evict inode cache entries that are no longer referenced by a live VFS
    /// user.  `INODE_CACHE` itself owns one strong Arc per key.  Callers clear
    /// the strong dentry cache first, so a count of one means this cache is the
    /// last owner.  This keeps LTP's short-lived pathname churn from becoming
    /// permanent kernel heap usage.
    pub fn reclaim_unused() -> usize {
        let capacity = INODE_CACHE.read().inodes.len();
        let mut reclaimed = Vec::with_capacity(capacity);
        {
            let mut cache = INODE_CACHE.write();
            cache.inodes.retain(|_, inode| {
                if Arc::strong_count(inode) > 1 {
                    return true;
                }
                // Retain one temporary Arc until after the index write lock is
                // released.  Ext4Inode::drop() enters EXT4_OP_LOCK.
                reclaimed.push(inode.clone());
                false
            });

            let InodeCacheState { paths, inodes } = &mut *cache;
            paths.retain(|_, key| inodes.contains_key(key));
        }
        let count = reclaimed.len();
        drop(reclaimed);
        count
    }

    fn reclaim_if_at_capacity() {
        let at_capacity = INODE_CACHE.read().inodes.len() >= MAX_CACHED_INODES;
        if at_capacity {
            // Positive dentries intentionally keep strong inode references for
            // hot close/open loops.  Drop that accelerator before testing which
            // FsIndex entries are otherwise idle.
            let cleared_dentries = crate::fs::DENTRY_CACHE.clear();
            let reclaimed_inodes = Self::reclaim_unused();
            #[cfg(feature = "perf")]
            crate::utils::perf::record_vfs_fsidx_reclaim(reclaimed_inodes, cleared_dentries);
            #[cfg(not(feature = "perf"))]
            let _ = (reclaimed_inodes, cleared_dentries);
        }
    }

    pub fn print_inner() {
        println!("{:#?}", INODE_CACHE.read().paths.keys());
    }

    fn cache_key(path: &str, inode: &Arc<dyn Inode>) -> InodeCacheKey {
        if is_proc_task_path(path) {
            return InodeCacheKey::Path(path.to_string());
        }
        if let Some((dev, ino)) = inode.cache_identity().filter(|(_, ino)| *ino != 0) {
            return InodeCacheKey::Inode { dev, ino };
        }
        let stat = inode.fstat();
        if stat.st_ino != 0 {
            InodeCacheKey::Inode {
                dev: stat.st_dev,
                ino: stat.st_ino,
            }
        } else {
            InodeCacheKey::Path(path.to_string())
        }
    }

    /// Publish a path under `key` and discard an old identity key once no path
    /// refers to it.  This prevents a changing lwext4 stat key from leaving a
    /// strong orphan in `inodes` that a later inode-number reuse can hit.
    fn bind_path(
        cache: &mut InodeCacheState,
        path: &str,
        key: &InodeCacheKey,
    ) -> Option<Arc<dyn Inode>> {
        let previous = cache.paths.insert(path.to_string(), key.clone());
        if let Some(previous) = previous {
            if &previous != key && !cache.paths.values().any(|candidate| candidate == &previous) {
                return cache.inodes.remove(&previous);
            }
        }
        None
    }

    /// A stale canonical inode can survive until the unlink-side cache detach
    /// runs.  Reusing it for a newly allocated ext4 inode would retain the old
    /// path and route I/O to the wrong file.  Hard-link aliases remain valid:
    /// Ext4Inode::fstat() recovers a live alias before reporting its identity.
    fn inode_matches_key(inode: &Arc<dyn Inode>, key: &InodeCacheKey) -> bool {
        match key {
            InodeCacheKey::Inode { dev, ino } => {
                // This is deliberately a live metadata probe rather than the
                // immutable lookup identity.  An unlinked inode can remain
                // cached until unlink-side detachment runs; if ext4 reuses
                // its number, only `fstat()` can reject that stale object.
                let stat = inode.fstat();
                stat.st_dev == *dev && stat.st_ino == *ino
            }
            InodeCacheKey::Path(path) if is_proc_task_path(path) => {
                inode.path() == path.as_str() && inode.fstat().st_ino != 0
            }
            InodeCacheKey::Path(path) => inode.path() == path.as_str(),
        }
    }
}

/// Per-process procfs entries are short lived and never participate in hard
/// links.  Keep them path-keyed so ext4's aggressively reused inode numbers
/// cannot merge a newly created proc entry with an unrelated regular file.
fn is_proc_task_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/proc/") else {
        return false;
    };
    let pid = rest.split_once('/').map_or(rest, |(pid, _)| pid);
    !pid.is_empty() && pid.as_bytes().iter().all(|byte| byte.is_ascii_digit())
}
