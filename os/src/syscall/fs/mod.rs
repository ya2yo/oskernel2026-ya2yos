mod ctl;
mod event;
mod fcntl;
mod fd_ops;
mod io;
mod mount;
mod pipe;
mod stat;

use linux_raw_sys::ctypes::c_int;
use log::warn;

use crate::{
    fs::{DummyFd, FileDescriptor, OpenFlags},
    task::current_task,
    utils::SyscallRet,
};

pub use self::{
    ctl::*, event::*, fcntl::*, fd_ops::*, io::*, mount::*, pipe::*,
    stat::*,
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

/// https://man7.org/linux/man-pages/man2/inotify_init1.2.html
pub fn sys_inotify_init1(_flags: u32) -> SyscallRet {
    warn!("[sys_inotify_init1] not implement!");
    dummyfd_create()
}
/// https://man7.org/linux/man-pages/man2/inotify_add_watch.2.html
pub fn sys_inotify_add_watch(_fd: c_int, _path: *const u8, _mask: u32)-> SyscallRet {
    warn!("[sys_inotify_add_watch] not implement!");
    Ok(0)
}
pub fn sys_inotify_rm_watch(_fd: c_int, _wd: c_int) -> SyscallRet {
    warn!("[sys_inotify_rm_watch] not implement!");
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/bpf.2.html
pub fn sys_bpf(_cmd: i32, _attr: *mut u8, _size: u32) -> SyscallRet {
    warn!("[sys_bpf] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/close_range.2.html
pub fn sys_close_range(first: u32, last: u32, flags: i32) -> SyscallRet {
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/io_uring_setup.2.html
pub fn sys_io_uring_setup(_entriers: u32, _params: *mut u8)->SyscallRet {
    warn!("[sys_io_uring_setup] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/memfd_create.2.html
pub fn sys_memfd_create(_name: *const u8, _flags: u32)->SyscallRet {
    warn!("[sys_memfd_create] not implement!");
    dummyfd_create()
}

/// https://www.man7.org/linux/man-pages//man2/memfd_secret.2.html
pub fn sys_memfd_secret(_flags: u32) -> SyscallRet {
    warn!("[sys_memfd_secret] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/perf_event_open.2.html
pub fn sys_perf_event_open(_attr: *mut u8, _pid: u32, _cpu: c_int, _group_fd: c_int, _flags: u32) -> SyscallRet {
    warn!("[sys_perf_event_open] not implement!");
    dummyfd_create()
}

/// 参考 https://man7.org/linux/man-pages/man2/signalfd4.2.html
pub fn sys_signalfd4(_siglfd: u32, _mask: *const u8, _flags: u32) -> SyscallRet {
    warn!("[sys_signalfd4] not implement!");
    dummyfd_create()
}

/// https://www.man7.org/linux/man-pages/man2/timerfd_create.2.html
pub fn sys_timerfd_create(_clockid: u32, _flags: u32) -> SyscallRet {
    warn!("[sys_timerfd_create] not implement!");
    dummyfd_create()
}
pub fn sys_timerfd_settime(
    _fd: u32,
    _flags: u32,
    _new_value: *const u8,
    _old_value: *mut u8,
) -> SyscallRet {
    warn!("[sys_timerfd_settime] not implement!");
    Ok(0)
}
pub fn sys_timerfd_gettime(_fd: u32, _curr_value: *mut u8) -> SyscallRet {
    warn!("[sys_timerfd_gettime] not implement!");
    Ok(0)
}
