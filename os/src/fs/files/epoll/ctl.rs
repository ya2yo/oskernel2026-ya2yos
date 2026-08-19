//! `EPOLL_CTL_ADD` / `MOD` / `DEL` 语义。
//!
//! 兴趣集合由互斥保护的 `BTreeMap` 保存，键是被监视的进程内 fd。控制操作
//! 只更新注册信息，不主动轮询目标文件；真正的就绪状态在等待路径中按需计算。

use super::file::{EpollEntry, EpollFile};
use crate::utils::SysErrNo;

impl EpollFile {
    /// 判断文件描述符是否已经注册在当前 epoll 实例中，不修改兴趣列表。
    pub fn contains_fd(&self, fd: i32) -> bool {
        self.interests.lock().contains_key(&fd)
    }

    /// 判断当前 epoll 实例的兴趣集合是否为空。
    pub fn is_interests_empty(&self) -> bool {
        self.interests.lock().is_empty()
    }

    /// 将一个文件描述符及其兴趣掩码加入当前 epoll 实例。
    ///
    /// `data` 会原样保存在兴趣项中，并在事件就绪时返回给用户态；若 `fd`
    /// 已存在，则返回 [`SysErrNo::EEXIST`]，不会覆盖原有注册项。
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

    /// 更新已注册文件描述符的兴趣掩码和用户数据。
    ///
    /// 修改不存在的 `fd` 返回 [`SysErrNo::ENOENT`]；更新后下一次轮询将
    /// 使用新的事件掩码和 `data`。
    pub fn ctl_mod(&self, fd: i32, events: u32, data: u64) -> Result<(), SysErrNo> {
        let mut interests = self.interests.lock();
        let entry = interests.get_mut(&fd).ok_or(SysErrNo::ENOENT)?;
        entry.events = events;
        entry.data = data;
        Ok(())
    }

    /// 从兴趣列表中删除一个已注册的文件描述符。
    ///
    /// 删除不存在的 `fd` 返回 [`SysErrNo::ENOENT`]。
    pub fn ctl_del(&self, fd: i32) -> Result<(), SysErrNo> {
        if self.interests.lock().remove(&fd).is_none() {
            return Err(SysErrNo::ENOENT);
        }
        Ok(())
    }
}
