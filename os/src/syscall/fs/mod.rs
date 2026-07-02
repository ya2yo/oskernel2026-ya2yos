mod ctl;
mod event;
mod fcntl;
mod fd_ops;
mod file_lock;
mod handle;
mod io;
mod mount;
mod mqueue;
mod pipe;
mod stat;
mod xattr;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use linux_raw_sys::ctypes::c_int;
use log::warn;

use crate::{
    fs::{DummyFd, File, FileClass, FileDescriptor, InotifyFd, OpenFlags},
    mm::{copy_from_user, read_user_cstr, UserBuffer},
    syscall::options::Iovec,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

// inotify_init1 标志 — 与 O_CLOEXEC / O_NONBLOCK 值相同
const IN_CLOEXEC: u32 = OpenFlags::O_CLOEXEC.bits();
const IN_NONBLOCK: u32 = OpenFlags::O_NONBLOCK.bits();

pub use self::{
    ctl::*, event::*, fcntl::*, fd_ops::*, handle::*, io::*, mount::*, mqueue::*, pipe::*, stat::*,
    xattr::*,
};

fn dummyfd_create() -> SyscallRet {
    let dummy_file = DummyFd::new();
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let newfd = proc_inner.fd_table.alloc_fd()?;
    proc_inner.fd_table.set(
        newfd,
        FileDescriptor::new(OpenFlags::empty(), crate::fs::FileClass::Abs(dummy_file)),
    );
    Ok(newfd)
}

/// https://man7.org/linux/man-pages/man2/inotify_init1.2.html
pub fn sys_inotify_init1(flags: u32) -> SyscallRet {
    // 只允许 IN_CLOEXEC 和 IN_NONBLOCK 两个标志
    let valid_flags = IN_CLOEXEC | IN_NONBLOCK;
    if flags & !valid_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let inotify_file = InotifyFd::new();
    if flags & IN_NONBLOCK != 0 {
        inotify_file.set_nonblocking(true)?;
    }

    let mut open_flags = OpenFlags::O_RDWR;
    if flags & IN_CLOEXEC != 0 {
        open_flags |= OpenFlags::O_CLOEXEC;
    }
    if flags & IN_NONBLOCK != 0 {
        open_flags |= OpenFlags::O_NONBLOCK;
    }

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let fd = proc_inner.fd_table.alloc_fd()?;
    proc_inner.fd_table.set(
        fd,
        FileDescriptor::new(open_flags, FileClass::Abs(inotify_file.clone())),
    )?;
    // 注册到全局表，供 add_watch / rm_watch 查找
    InotifyFd::register_fd(fd, &inotify_file);
    Ok(fd)
}

/// https://man7.org/linux/man-pages/man2/inotify_add_watch.2.html
pub fn sys_inotify_add_watch(fd: c_int, path: *const u8, mask: u32) -> SyscallRet {
    if fd < 0 {
        return Err(SysErrNo::EBADF);
    }
    let fd = fd as usize;
    let inotify = InotifyFd::lookup(fd)?;

    // 从用户空间读取路径字符串
    let path_str = {
        let task = current_task().unwrap();
        let process = &task.process;
        let memory_set = process.memory_set_arc();
        read_user_cstr(&memory_set, path)?
    };

    if mask == 0 {
        return Err(SysErrNo::EINVAL);
    }

    inotify.add_watch(path_str, mask)
}

pub fn sys_inotify_rm_watch(fd: c_int, wd: c_int) -> SyscallRet {
    if fd < 0 {
        return Err(SysErrNo::EBADF);
    }
    let fd = fd as usize;
    let inotify = InotifyFd::lookup(fd)?;
    inotify.rm_watch(wd)
}

/// https://man7.org/linux/man-pages/man2/bpf.2.html
pub fn sys_bpf(_cmd: i32, _attr: *mut u8, _size: u32) -> SyscallRet {
    warn!("[sys_bpf] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/io_uring_setup.2.html
pub fn sys_io_uring_setup(_entriers: u32, _params: *mut u8) -> SyscallRet {
    warn!("[sys_io_uring_setup] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/memfd_create.2.html
pub fn sys_memfd_create(_name: *const u8, _flags: u32) -> SyscallRet {
    warn!("[sys_memfd_create] not implement!");
    dummyfd_create()
}

/// https://www.man7.org/linux/man-pages//man2/memfd_secret.2.html
pub fn sys_memfd_secret(_flags: u32) -> SyscallRet {
    warn!("[sys_memfd_secret] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/perf_event_open.2.html
pub fn sys_perf_event_open(
    _attr: *mut u8,
    _pid: u32,
    _cpu: c_int,
    _group_fd: c_int,
    _flags: u32,
) -> SyscallRet {
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
