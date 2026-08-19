//! 维护 epoll 实例索引（`Arc<dyn File>` 无法 downcast）。
//!
//! 文件描述符表只暴露 `Arc<dyn File>`，因此无法直接将对象向下转型为
//! `EpollFile`。全局表以对象地址为键、以弱引用为值：创建时登记，查找时
//! 通过当前进程的 fd 表确认对象身份，并在条目失效后清理弱引用。

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
    /// 将新创建的 epoll 实例登记到全局弱引用索引中。
    ///
    /// 索引不持有实例的强引用，因此不会改变实例的生命周期；实例销毁后，
    /// 失效条目会在后续清理时移除。
    pub fn register_instance(ep: &Arc<EpollFile>) {
        let key = Arc::as_ptr(ep) as usize;
        EPOLL_TABLE.lock().insert(key, Arc::downgrade(ep));
    }

    /// 校验当前进程的 `epfd` 指向一个已注册的 epoll 实例。
    ///
    /// 方法先从调用进程的文件描述符表取得对象，再通过指针身份与全局索引
    /// 中的实例比对；描述符无效或对象不是 epoll 实例时返回 [`SysErrNo::EBADF`]。
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

    /// 清理实例已销毁后留下的弱引用索引项。
    fn prune_dead_entries() {
        EPOLL_TABLE.lock().retain(|_, w| w.upgrade().is_some());
    }
}

impl Drop for EpollFile {
    fn drop(&mut self) {
        EpollFile::prune_dead_entries();
    }
}
