//! epoll 实例：兴趣列表 + [`File`] trait。

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::task::Context;
use linux_raw_sys::general::EPOLL_CLOEXEC;
use spin::Mutex;

use super::events::{epoll_events_to_poll, poll_to_epoll_events};
use crate::{
    fs::{vfs::File, Kstat},
    syscall::PollEvents,
    utils::PollSet,
};

bitflags! {
    pub struct EpollCreateFlags: u32 {
        const CLOEXEC = EPOLL_CLOEXEC;
    }
}

pub(crate) struct EpollEntry {
    pub events: u32,
    pub data: u64,
    pub last_events: u32,
}

/// 每个 `epoll_create1` 对应一个实例，以 `FileClass::Abs` 存入 fd 表。
pub struct EpollFile {
    pub(crate) interests: Mutex<BTreeMap<i32, EpollEntry>>,
    _poll_set: PollSet,
}

impl EpollFile {
    pub fn new() -> Self {
        Self {
            interests: Mutex::new(BTreeMap::new()),
            _poll_set: PollSet::new(),
        }
    }

    /// 对被监视 fd 调用 `File::poll`，返回当前 epoll 事件掩码。
    pub fn poll_mask(file: &dyn File, registered_events: u32) -> u32 {
        let pe = epoll_events_to_poll(registered_events);
        poll_to_epoll_events(file.poll(pe))
    }

    /// Register a waiter with the underlying file for an epoll interest mask.
    pub fn register_mask(
        file: &dyn File,
        registered_events: u32,
        context: &mut Context<'_>,
    ) {
        file.register(context, epoll_events_to_poll(registered_events));
    }

    pub(crate) fn snapshot_interests(&self) -> Vec<(i32, EpollEntry)> {
        self.interests
            .lock()
            .iter()
            .map(|(&fd, entry)| {
                (
                    fd,
                    EpollEntry {
                        events: entry.events,
                        data: entry.data,
                        last_events: entry.last_events,
                    },
                )
            })
            .collect()
    }
}

impl File for EpollFile {
    fn readable(&self) -> bool {
        false
    }
    fn writable(&self) -> bool {
        false
    }
    fn fstat(&self) -> Kstat {
        Kstat::default()
    }

    fn poll(&self, _events: PollEvents) -> PollEvents {
        PollEvents::empty()
    }
}
