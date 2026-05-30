//! 收集就绪事件（水平/边缘触发、ONESHOT、已关闭 fd）。

use alloc::vec::Vec;
use linux_raw_sys::general::{EPOLLERR, EPOLLET, EPOLLHUP, EPOLLONESHOT};

use super::file::EpollFile;

/// 单次 `epoll_wait` 返回给用户态的一条记录。
pub struct EpollReady {
    pub events: u32,
    pub data: u64,
}

impl EpollFile {
    /// 轮询兴趣列表。
    ///
    /// `poll_one(fd, registered_events)` 返回 `None` 表示 fd 已关闭。
    pub fn collect_ready<F>(&self, poll_one: &mut F, maxevents: usize) -> Vec<EpollReady>
    where
        F: FnMut(i32, u32) -> Option<u32>,
    {
        let interests = self.snapshot_interests();
        if interests.is_empty() || maxevents == 0 {
            return Vec::new();
        }

        let mut ready = Vec::new();
        let mut interests_lock = self.interests.lock();
        let mut to_remove: Vec<i32> = Vec::new();

        for (fd, entry) in interests {
            if ready.len() >= maxevents {
                break;
            }

            let Some(current) = poll_one(fd, entry.events) else {
                let revents = EPOLLERR | EPOLLHUP;
                ready.push(EpollReady {
                    events: (revents & entry.events) | (revents & EPOLLERR),
                    data: entry.data,
                });
                to_remove.push(fd);
                continue;
            };

            let is_et = entry.events & EPOLLET != 0;
            let fired = if is_et {
                current & !entry.last_events
            } else {
                current
            };

            if let Some(real_entry) = interests_lock.get_mut(&fd) {
                real_entry.last_events = current;
            }

            let forced = fired & (EPOLLERR | EPOLLHUP);
            let matched = fired & entry.events;
            let to_report = matched | forced;

            if to_report == 0 {
                continue;
            }

            ready.push(EpollReady {
                events: to_report,
                data: entry.data,
            });

            if entry.events & EPOLLONESHOT != 0 {
                to_remove.push(fd);
            }
        }

        for fd in to_remove {
            interests_lock.remove(&fd);
        }

        ready
    }
}
