//! 这个模块专门存放 File 的各个 Impl

pub mod devfs;
#[cfg(feature = "net")]
mod net;
pub mod pipe;
pub mod stdio;
#[cfg(feature = "net")]
pub use net::*;
mod os_file;
mod epoll;
mod events;
mod signalfd;
mod dummyfd;
pub use {
    dummyfd::DummyFd,
    events::EventFd,
    epoll::{EpollCreateFlags, EpollFile, EpollReady},
    os_file::OSFile,
};