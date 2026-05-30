//! `EPOLL_CTL_ADD` / `MOD` / `DEL` 语义。

use super::file::{EpollEntry, EpollFile};
use crate::utils::SysErrNo;

impl EpollFile {
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
