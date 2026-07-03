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
    Inode { dev: usize, ino: usize },
    Path(String),
}

static PATH_INDEX: Lazy<RwLock<HashMap<String, InodeCacheKey>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
static INODE_CACHE: Lazy<RwLock<HashMap<InodeCacheKey, Arc<dyn Inode>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
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
