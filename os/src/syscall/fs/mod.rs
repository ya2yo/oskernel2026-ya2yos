mod ctl;
mod event;
mod fcntl;
mod fd_ops;
mod io;
mod mman;
mod mount;
mod pidfd;
mod pipe;
mod signalfd;
mod stat;
mod timerfd;
mod inotify;
mod perf_event;
mod liburing;
mod bpf;
use crate::{
    fs::{DummyFd, FileDescriptor, OpenFlags},
    task::current_task,
    utils::SyscallRet,
};

pub use self::{
    ctl::*, event::*, fcntl::*, fd_ops::*, io::*, mman::*, mount::*, pidfd::*, pipe::*,
    signalfd::*, stat::*, timerfd::*, inotify::*, perf_event::*, liburing::*,bpf::*
};

fn dummyfd_create() -> SyscallRet {
    let dummy_file = DummyFd::new();
    let task = current_task().unwrap();
    let fd_table = task.get_fd_table();
    let newfd = fd_table.alloc_fd()?;
    task.get_fd_table()
        .set(newfd, FileDescriptor::new(OpenFlags::empty(), crate::fs::FileClass::Abs(dummy_file)));
    Ok(newfd)
}