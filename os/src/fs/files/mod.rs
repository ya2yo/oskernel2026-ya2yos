//! 文件对象实现集合。
//!
//! 本模块集中放置 VFS [`File`] trait 的具体实现，并在末尾统一导出供文件描述符
//! 表和 syscall 层使用。各子模块分别覆盖普通 inode 文件、管道、设备、epoll、
//! eventfd、信号 fd、定时器 fd、proc 动态文件以及匿名内存文件等类型。
//!
//! `mod`/`pub mod` 的可见性是有意设计的：实现细节留在本模块内部，调用方只
//! 依赖这里重新导出的对象和 ABI 结构，从而避免 syscall 层直接耦合内部状态。

pub mod devfs;
pub mod loopdev;
#[cfg(feature = "net")]
mod net;
pub mod pipe;
pub mod stdio;
#[cfg(feature = "net")]
pub use net::*;
pub mod bpf;
mod dummyfd;
mod epoll;
mod events;
mod fanotify;
mod inotify;
mod io_uring;
mod mountfd;
pub mod mqueue;
mod os_file;
mod pagemap;
mod secretmem;
mod signalfd;
mod timerfd;
mod tmp_file;
mod uptime;
pub use {
    dummyfd::DummyFd,
    epoll::{EpollCreateFlags, EpollFile, EpollReady},
    events::EventFd,
    fanotify::{
        fanotify_events_suppressed, notify_path_event, suppress_fanotify_events, FanotifyFd,
        FAN_ACCESS, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_MODIFY, FAN_OPEN,
    },
    inotify::{InotifyFd, InotifyMask},
    io_uring::{IoCqringOffsets, IoSqringOffsets, IoUringFd, IoUringParams, IORING_MAX_ENTRIES},
    mountfd::{DetachedMountFd, FsConfigOption, FsConfigValue, FsContext, FsContextFd},
    mqueue::{MqAttr, Mqueue},
    os_file::OSFile,
    pagemap::PagemapFile,
    secretmem::SecretMemFile,
    signalfd::SignalFd,
    timerfd::{TimerFd, TimerFdSpec},
    tmp_file::TmpFile,
    uptime::UptimeFile,
};
