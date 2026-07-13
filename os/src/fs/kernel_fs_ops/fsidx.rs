use alloc::{
    string::{String, ToString},
    sync::Arc,
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

/// 路径别名索引：`绝对路径 -> inode 身份键`。
///
/// 同一个 inode 的硬链接会对应多个路径条目，但会指向同一个 key。它承担了
/// 部分 Linux dcache 的路径关联职责；完整的父目录/子名称 dentry 缓存见
/// `DENTRY_CACHE`，本表不保存 positive/negative dentry 状态。
static PATH_INDEX: Lazy<RwLock<HashMap<String, InodeCacheKey>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// VFS inode 对象缓存：`inode 身份键 -> 规范的 inode 对象`。
///
/// 插入同一 `(st_dev, st_ino)` 时复用已有 `Arc<dyn Inode>`，从而使硬链接和
/// 已解析的路径别名共享同一个 VFS inode 对象。概念上对应 Linux 的全局
/// `inode_hashtable`，其内部以 `(super_block, i_ino)` 标识 inode。
static INODE_CACHE: Lazy<RwLock<HashMap<InodeCacheKey, Arc<dyn Inode>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// 特殊节点类型补充表：`绝对路径 -> InodeType`。
///
/// 用于 FIFO、设备节点和路径名 UNIX socket 等节点，在底层 inode 无法稳定提供
/// 类型时保留其创建时的类型。Linux 不使用独立表，而是将此信息编码在
/// `struct inode::i_mode` 的 `S_IF*` 类型位中。
static SPECIAL_NODE_TYPES: Lazy<RwLock<HashMap<String, InodeType>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

pub struct FsIndex;

impl FsIndex {
    pub fn has_inode(path: &str) -> bool {
        Self::find_inode_idx(path).is_some()
    }

    pub fn find_inode_idx(path: &str) -> Option<Arc<dyn Inode>> {
        let key = PATH_INDEX.read().get(path).cloned()?;
        let inode = INODE_CACHE.read().get(&key).cloned()?;
        inode.cache_path_alias(path);
        Some(inode)
    }

    pub fn insert_inode_idx(path: &str, inode: Arc<dyn Inode>) -> Arc<dyn Inode> {
        let key = Self::cache_key(path, &inode);
        let canonical = {
            let mut cache = INODE_CACHE.write();
            if let Some(existing) = cache.get(&key).cloned() {
                existing.cache_path_alias(path);
                existing
            } else {
                inode.cache_path_alias(path);
                cache.insert(key.clone(), inode.clone());
                inode
            }
        };
        PATH_INDEX.write().insert(path.to_string(), key);
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
        if let Some(key) = PATH_INDEX.write().remove(path) {
            INODE_CACHE.write().remove(&key);
        }
        SPECIAL_NODE_TYPES.write().remove(path);
    }

    pub fn print_inner() {
        println!("{:#?}", PATH_INDEX.read().keys());
    }

    fn cache_key(path: &str, inode: &Arc<dyn Inode>) -> InodeCacheKey {
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
}
