//! epoll 文件对象：兴趣列表、就绪收集、全局 fd 索引。
//!
//! 系统调用入口在 [`crate::syscall::io_mpx::epoll`]。

mod ctl;
mod events;
mod file;
mod registry;
mod wait;

pub use file::{EpollCreateFlags, EpollFile};
pub use wait::EpollReady;
