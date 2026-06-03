//! `EPOLL_CTL_ADD` / `MOD` / `DEL` 语义。

use super::file::{EpollEntry, EpollFile};
use crate::utils::SysErrNo;

impl EpollFile {
    /// Check if a fd is already registered in the epoll set (non-modifying).
    pub fn contains_fd(&self, fd: i32) -> bool {
        self.interests.lock().contains_key(&fd)
    }

    /// Check if the interest set is empty.
    pub fn is_interests_empty(&self) -> bool {
        self.interests.lock().is_empty()
    }

    pub fn ctl_add(&self, fd: i32, events: u32, data: u64) -> Result<(), SysErrNo> {
        let mut interests = self.interests.lock();
        if interests.contains_key(&fd) {
            return Err(SysErrNo::EEXIST);
        }
        interests.insert(
            fd,
            EpollEntry {
                events,
                data,
                last_events: 0,
            },
        );
        Ok(())
    }

    pub fn ctl_mod(&self, fd: i32, events: u32, data: u64) -> Result<(), SysErrNo> {
        let mut interests = self.interests.lock();
        let entry = interests.get_mut(&fd).ok_or(SysErrNo::ENOENT)?;
        entry.events = events;
        entry.data = data;
        Ok(())
    }

    pub fn ctl_del(&self, fd: i32) -> Result<(), SysErrNo> {
        if self.interests.lock().remove(&fd).is_none() {
            return Err(SysErrNo::ENOENT);
        }
        Ok(())
    }
}
