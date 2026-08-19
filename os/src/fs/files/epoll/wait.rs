//! 收集就绪事件（水平/边缘触发、ONESHOT、已关闭 fd）。
//!
//! 等待逻辑先复制兴趣项快照，再逐项调用回调查询目标 fd，避免在轮询过程中
//! 长时间持有兴趣锁。边缘触发通过 `current & !last_events` 只报告新出现的
//! 状态；错误和挂断始终强制报告，`EPOLLONESHOT` 返回一次后移除注册项。

use alloc::vec::Vec;
use linux_raw_sys::general::{EPOLLERR, EPOLLET, EPOLLHUP, EPOLLONESHOT};

use super::file::EpollFile;

/// 单次 `epoll_wait` 返回给用户态的一条记录。
pub struct EpollReady {
    /// 本次轮询匹配到的 Linux `epoll` 事件掩码。
    pub events: u32,
    /// 注册兴趣项携带的用户数据，原样返回给调用者。
    pub data: u64,
}

impl EpollFile {
    /// 遍历兴趣列表并收集本次调用中已就绪的事件。
    ///
    /// `poll_one` 负责查询目标文件当前状态，并以 `None` 表示目标文件已关闭。
    /// 普通模式报告当前就绪状态，`EPOLLET` 只报告相对上次轮询新增的状态，
    /// `EPOLLONESHOT` 项在报告后移除；结果数量不会超过 `maxevents`。
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
