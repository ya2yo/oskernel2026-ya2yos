use alloc::{
    string::{String, ToString},
    sync::{Arc, Weak},
};
use hashbrown::HashMap;
use spin::{Lazy, RwLock};

use super::Inode;

#[derive(Clone, Eq, Hash, PartialEq)]
struct DentryKey {
    parent: usize,
    name: String,
}

enum DentryValue {
    Positive { inode: Weak<dyn Inode> },
    Negative,
}

pub enum DentryLookup {
    Positive(Arc<dyn Inode>),
    Negative,
}

pub struct DentryCache {
    entries: RwLock<HashMap<DentryKey, DentryValue>>,
}

impl DentryCache {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    pub fn lookup(&self, parent: &Arc<dyn Inode>, name: &str) -> Option<DentryLookup> {
        let key = Self::key(parent, name);
        let stale_positive = {
            let entries = self.entries.read();
            match entries.get(&key) {
                Some(DentryValue::Positive { inode }) => match inode.upgrade() {
                    Some(inode) => return Some(DentryLookup::Positive(inode)),
                    None => true,
                },
                Some(DentryValue::Negative) => return Some(DentryLookup::Negative),
                None => return None,
            }
        };

        if stale_positive {
            self.entries.write().remove(&key);
        }
        None
    }

    pub fn insert_positive(&self, parent: &Arc<dyn Inode>, name: &str, inode: Arc<dyn Inode>) {
        self.entries.write().insert(
            Self::key(parent, name),
            DentryValue::Positive {
                inode: Arc::downgrade(&inode),
            },
        );
    }

    pub fn insert_negative(&self, parent: &Arc<dyn Inode>, name: &str) {
        self.entries
            .write()
            .insert(Self::key(parent, name), DentryValue::Negative);
    }

    pub fn invalidate(&self, parent: &Arc<dyn Inode>, name: &str) {
        self.entries.write().remove(&Self::key(parent, name));
    }

    pub fn invalidate_parent(&self, parent: &Arc<dyn Inode>) {
        let parent = Self::parent_key(parent);
        self.entries.write().retain(|key, _| key.parent != parent);
    }

    fn key(parent: &Arc<dyn Inode>, name: &str) -> DentryKey {
        DentryKey {
            parent: Self::parent_key(parent),
            name: name.to_string(),
        }
    }

    fn parent_key(parent: &Arc<dyn Inode>) -> usize {
        Arc::as_ptr(parent) as *const () as usize
    }
}

pub static DENTRY_CACHE: Lazy<DentryCache> = Lazy::new(DentryCache::new);
