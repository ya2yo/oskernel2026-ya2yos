//! 按 epoll fd 索引 [`EpollFile`]（`Arc<dyn File>` 无法 downcast）。

use alloc::{collections::BTreeMap, sync::Arc, sync::Weak, vec::Vec};
use spin::{Lazy, Mutex};

use super::file::EpollFile;
use crate::utils::SysErrNo;

static EPOLL_TABLE: Lazy<Mutex<BTreeMap<usize, Weak<EpollFile>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

impl EpollFile {
    /// `epoll_create1` 分配 fd 后注册。
    pub fn register_fd(epfd: usize, ep: &Arc<EpollFile>) {
        EPOLL_TABLE.lock().insert(epfd, Arc::downgrade(ep));
    }

    /// 校验 `epfd` 指向 epoll 实例。
    pub fn lookup(epfd: usize) -> Result<Arc<EpollFile>, SysErrNo> {
        EPOLL_TABLE
            .lock()
            .get(&epfd)
            .and_then(|w| w.upgrade())
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
