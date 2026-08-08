mod ctl;
mod event;
mod fanotify;
mod fcntl;
mod fd_ops;
pub(crate) mod file_lock;
mod handle;
mod inotify;
mod misc;
mod mount;
mod mqueue;
mod path;
mod pipe;
mod space;
mod stat;
mod xattr;

use linux_raw_sys::{
    ctypes::c_int,
    general::{MFD_ALLOW_SEALING, MFD_CLOEXEC, MFD_HUGETLB, MFD_NOEXEC_SEAL},
};
use log::warn;

use crate::{
    fs::{DummyFd, FileClass, FileDescriptor, OpenFlags, TmpFile},
    mm::{if_bad_address, read_user_cstr_with_limit},
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

pub use self::{
    ctl::*, event::*, fanotify::*, fcntl::*, fd_ops::*, handle::*, inotify::*, misc::*, mount::*,
    mqueue::*, path::*, pipe::*, space::*, stat::*, xattr::*,
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

/// https://man7.org/linux/man-pages/man2/io_uring_setup.2.html
pub fn sys_io_uring_setup(_entriers: u32, _params: *mut u8) -> SyscallRet {
    warn!("[sys_io_uring_setup] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/memfd_create.2.html
pub fn sys_memfd_create(name: *const u8, flags: u32) -> SyscallRet {
    const MFD_KNOWN_FLAGS: u32 = MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_HUGETLB | MFD_NOEXEC_SEAL;
    const MAX_MEMFD_NAME: usize = 249;

    if flags & !MFD_KNOWN_FLAGS != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if flags & MFD_HUGETLB != 0 {
        // This kernel has no hugetlb-backed file implementation yet.
        return Err(SysErrNo::EINVAL);
    }
    if name.is_null() || if_bad_address(name as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let memory_set = task.process.memory_set_arc();
    let _name_bytes =
        read_user_cstr_with_limit(&memory_set, name, MAX_MEMFD_NAME + 1).map_err(|err| {
            if err == SysErrNo::E2BIG {
                SysErrNo::ENAMETOOLONG
            } else {
                err
            }
        })?;
    let inner = task.inner_lock();
    let uid = inner.effective_uid;
    let gid = inner.effective_gid;
    drop(inner);
    let file = FileClass::Abs(TmpFile::new(true, true, 0o666, uid, gid));
    let fd_table = &task.process.fd_table;
    let fd = fd_table.alloc_fd()?;
    let open_flags = if flags & MFD_CLOEXEC != 0 {
        OpenFlags::O_CLOEXEC
    } else {
        OpenFlags::empty()
    };
    fd_table.set(fd, FileDescriptor::new(open_flags, file));
    Ok(fd)
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
