// !Deprecated
use alloc::{
    string::{String, ToString},
    sync::Arc,
};
use hashbrown::HashMap;
use spin::{Lazy, RwLock};

use super::{Inode, InodeType};

static FSIDX: Lazy<RwLock<HashMap<String, Arc<dyn Inode>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
static SPECIAL_NODE_TYPES: Lazy<RwLock<HashMap<String, InodeType>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

pub struct FsIndex;

impl FsIndex {
    pub fn has_inode(path: &str) -> bool {
        FSIDX.read().contains_key(path)
    }

    pub fn find_inode_idx(path: &str) -> Option<Arc<dyn Inode>> {
        FSIDX.read().get(path).map(Arc::clone)
    }

    pub fn insert_inode_idx(path: &str, inode: Arc<dyn Inode>) {
        FSIDX.write().insert(path.to_string(), inode);
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
        FSIDX.write().remove(path);
        SPECIAL_NODE_TYPES.write().remove(path);
    }

    pub fn print_inner() {
        println!("{:#?}", FSIDX.read().keys());
    }
}
