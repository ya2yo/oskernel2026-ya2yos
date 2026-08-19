//! epoll 实例：兴趣列表 + [`File`] trait。
//!
//! `EpollFile` 本身不是可读写的数据流，而是一个将多个文件对象聚合为
//! 就绪事件的文件描述符。`interests` 保存注册项，`_poll_set` 为等待者
//! 预留统一的唤醒接口；目标文件的实际状态通过 `File::poll` 查询。

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

/// `epoll_create1` 支持的实例创建选项。
///
/// 当前实现支持 [`EPOLL_CLOEXEC`](linux_raw_sys::general::EPOLL_CLOEXEC)，
/// 用于控制实例文件描述符是否在执行新程序时自动关闭。
bitflags! {
    pub struct EpollCreateFlags: u32 {
        const CLOEXEC = EPOLL_CLOEXEC;
    }
}

/// 一个被 epoll 监视的 fd 的注册信息。
///
/// `events` 是用户请求的兴趣掩码，`data` 是返回事件时携带的不透明数据；
/// `last_events` 保存上次轮询结果，供 `EPOLLET` 计算新出现的边沿。
pub(crate) struct EpollEntry {
    /// 用户注册的事件掩码。
    pub events: u32,
    /// 用户态关联数据，事件返回时原样带回。
    pub data: u64,
    /// 上一次观察到的目标文件事件。
    pub last_events: u32,
}

/// 每个 `epoll_create1` 对应一个实例，以 `FileClass::Abs` 存入 fd 表。
pub struct EpollFile {
    /// 按被监视 fd 保存兴趣掩码、用户数据和边缘触发状态。
    pub(crate) interests: Mutex<BTreeMap<i32, EpollEntry>>,
    /// 为实例等待者预留的统一唤醒集合。
    _poll_set: PollSet,
}

impl EpollFile {
    /// 创建一个空的 epoll 实例。
    ///
    /// 新实例尚未注册任何目标文件；调用方通常会随后将其包装为文件对象
    /// 并插入当前进程的文件描述符表。
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

    /// 按 epoll 兴趣掩码向目标文件注册一个等待者。
    ///
    /// 该方法只负责把事件掩码转换为内核轮询格式并委托给目标文件，实际的
    /// 唤醒由目标文件的 [`File::register`] 实现负责。
    pub fn register_mask(file: &dyn File, registered_events: u32, context: &mut Context<'_>) {
        file.register(context, epoll_events_to_poll(registered_events));
    }

    /// 在不持有兴趣列表锁的情况下取得一份兴趣项快照。
    ///
    /// 快照保留文件描述符、注册掩码、用户数据以及边缘触发所需的上次
    /// 事件状态，供就绪收集逻辑遍历；后续状态更新仍写回原兴趣列表。
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
