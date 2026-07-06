use linux_raw_sys::ctypes::c_int;

use crate::{
    fs::{File, FileClass, FileDescriptor, InotifyFd, OpenFlags},
    mm::read_user_cstr,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

// inotify_init1 标志 — 与 O_CLOEXEC / O_NONBLOCK 值相同
const IN_CLOEXEC: u32 = OpenFlags::O_CLOEXEC.bits();
const IN_NONBLOCK: u32 = OpenFlags::O_NONBLOCK.bits();

/// https://man7.org/linux/man-pages/man2/inotify_init1.2.html
pub fn sys_inotify_init1(flags: u32) -> SyscallRet {
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
