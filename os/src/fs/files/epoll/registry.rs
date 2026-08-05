//! 维护 epoll 实例索引（`Arc<dyn File>` 无法 downcast）。

use alloc::{collections::BTreeMap, sync::Arc, sync::Weak};
use spin::{Lazy, Mutex};

use super::file::EpollFile;
use crate::{fs::FdTable, utils::SysErrNo};

// fd numbers are local to a process, so a raw fd cannot be a global key.
// Use the EpollFile allocation address instead and resolve the current fd
// through its process fd table before returning an instance.
static EPOLL_TABLE: Lazy<Mutex<BTreeMap<usize, Weak<EpollFile>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

impl EpollFile {
    /// `epoll_create1` 分配对象后注册。
    pub fn register_instance(ep: &Arc<EpollFile>) {
        let key = Arc::as_ptr(ep) as usize;
        EPOLL_TABLE.lock().insert(key, Arc::downgrade(ep));
    }

    /// 校验当前进程的 `epfd` 指向一个已注册的 epoll 实例。
    pub fn lookup(epfd: usize, fd_table: &FdTable) -> Result<Arc<EpollFile>, SysErrNo> {
        let file = fd_table.get(epfd)?.abs()?;
        let table = EPOLL_TABLE.lock();
        table
            .values()
            .filter_map(Weak::upgrade)
            .find(|ep| {
                let ep_file: Arc<dyn crate::fs::vfs::File> = ep.clone();
                Arc::ptr_eq(&file, &ep_file)
            })
            .ok_or(SysErrNo::EBADF)
    }

    fn prune_dead_entries() {
        EPOLL_TABLE.lock().retain(|_, w| w.upgrade().is_some());
    }
}

impl Drop for EpollFile {
    fn drop(&mut self) {
        EpollFile::prune_dead_entries();
    }
}
