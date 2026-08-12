mod ctl;
mod event;
mod fanotify;
mod fcntl;
mod fd_ops;
pub(crate) mod file_lock;
mod handle;
mod inotify;
mod memfd;
mod misc;
mod mount;
mod mqueue;
mod path;
mod pipe;
mod space;
mod stat;
mod xattr;

use linux_raw_sys::ctypes::c_int;
use log::warn;

use crate::{
    fs::{
        DummyFd, File, FileClass, FileDescriptor, IoCqringOffsets, IoSqringOffsets, IoUringFd,
        IoUringParams, OpenFlags, IORING_MAX_ENTRIES,
    },
    mm::{copy_from_user, copy_to_user, if_bad_address},
    signal::SigSet,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

pub use self::{
    ctl::*, event::*, fanotify::*, fcntl::*, fd_ops::*, handle::*, inotify::*, memfd::*, misc::*,
    mount::*, mqueue::*, path::*, pipe::*, space::*, stat::*, xattr::*,
};

const SFD_CLOEXEC: u32 = 0x80000;
const SFD_NONBLOCK: u32 = 0x800;

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
pub fn sys_io_uring_setup(entries: u32, params: *mut u8) -> SyscallRet {
    if entries == 0 || entries > IORING_MAX_ENTRIES {
        return Err(SysErrNo::EINVAL);
    }
    if params.is_null() || if_bad_address(params as usize) {
        return Err(SysErrNo::EFAULT);
    }

    // This backend does not expose setup flags yet. Keep the ABI-visible
    // parameter block deterministic so callers can inspect the ring geometry.
    let task = current_task().unwrap();
    let memory_set = task.process.memory_set_arc();
    let mut params_value = IoUringParams::default();
    let params_bytes = unsafe {
        core::slice::from_raw_parts_mut(
            &mut params_value as *mut IoUringParams as *mut u8,
            core::mem::size_of::<IoUringParams>(),
        )
    };
    copy_from_user(&memory_set, params as usize, params_bytes)?;
    if params_value.flags != 0 {
        return Err(SysErrNo::EINVAL);
    }
    params_value.sq_entries = entries.next_power_of_two();
    params_value.cq_entries = entries.next_power_of_two();
    params_value.sq_thread_cpu = 0;
    params_value.sq_thread_idle = 0;
    params_value.features = 0;
    params_value.wq_fd = 0;
    params_value.sq_off = IoSqringOffsets::default();
    params_value.cq_off = IoCqringOffsets::default();
    let params_bytes = unsafe {
        core::slice::from_raw_parts(
            &params_value as *const IoUringParams as *const u8,
            core::mem::size_of::<IoUringParams>(),
        )
    };
    copy_to_user(&memory_set, params as usize, params_bytes)?;

    let fd = task.process.fd_table.alloc_fd()?;
    task.process.fd_table.set(
        fd,
        FileDescriptor::new(OpenFlags::empty(), FileClass::Abs(IoUringFd::new())),
    );
    Ok(fd)
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
pub fn sys_signalfd4(siglfd: u32, mask: *const u8, flags: u32, sigsetsize: usize) -> SyscallRet {
    if sigsetsize != core::mem::size_of::<SigSet>() {
        return Err(SysErrNo::EINVAL);
    }
    if mask.is_null() || if_bad_address(mask as usize) {
        return Err(SysErrNo::EFAULT);
    }
    if flags & !(SFD_CLOEXEC | SFD_NONBLOCK) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().unwrap();
    let memory_set = task.process.memory_set_arc();
    let mut set = SigSet::empty();
    copy_from_user(&memory_set, mask as usize, unsafe {
        core::slice::from_raw_parts_mut(
            &mut set as *mut SigSet as *mut u8,
            core::mem::size_of::<SigSet>(),
        )
    })?;
    set.remove(SigSet::SIGKILL | SigSet::SIGSTOP);
    let mut open_flags = OpenFlags::O_RDONLY;
    if flags & SFD_CLOEXEC != 0 {
        open_flags |= OpenFlags::O_CLOEXEC;
    }
    if flags & SFD_NONBLOCK != 0 {
        open_flags |= OpenFlags::O_NONBLOCK;
    }
    if siglfd != u32::MAX {
        let desc = task.process.fd_table.get(siglfd as usize)?;
        if !desc.any().update_signal_mask(set) {
            return Err(SysErrNo::EINVAL);
        }
        return Ok(siglfd as usize);
    }
    let file = crate::fs::SignalFd::new(task.clone(), set);
    if flags & SFD_NONBLOCK != 0 {
        file.set_nonblocking(true)?;
    }
    let fd = task.process.fd_table.alloc_fd()?;
    task.process
        .fd_table
        .set(fd, FileDescriptor::new(open_flags, FileClass::Abs(file)))?;
    Ok(fd)
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
