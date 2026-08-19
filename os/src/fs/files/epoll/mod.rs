//! epoll 文件对象及其事件管理实现。
//!
//! 本模块将一个 `epoll` 实例表示为 [`EpollFile`]，并维护被监视文件描述符的
//! 兴趣列表、就绪事件收集和实例索引。事件掩码在 Linux `epoll` 格式与内核
//! [`crate::syscall::PollEvents`] 格式之间转换；系统调用入口位于
//! [`crate::syscall::io_mpx::epoll`]。

mod ctl;
mod events;
mod file;
mod registry;
mod wait;

pub use file::{EpollCreateFlags, EpollFile};
pub use wait::EpollReady;
