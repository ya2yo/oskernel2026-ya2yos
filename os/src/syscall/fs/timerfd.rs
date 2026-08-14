//! timerfd syscalls: ABI decoding and delegation to [`crate::fs::TimerFd`].

use crate::{
    fs::{FileClass, FileDescriptor, OpenFlags, TimerFd, TimerFdSpec},
    mm::{copy_from_user_val, copy_to_user_val},
    task::current_task,
    timer::Timespec,
    utils::{SysErrNo, SyscallRet},
};

const TFD_NONBLOCK: u32 = 0x800;
const TFD_CLOEXEC: u32 = 0x80000;
const TFD_TIMER_ABSTIME: u32 = 1;
const CLOCK_REALTIME: u32 = 0;
const CLOCK_MONOTONIC: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ITimerSpec {
    it_interval: Timespec,
    it_value: Timespec,
}

/// https://www.man7.org/linux/man-pages/man2/timerfd_create.2.html
pub fn sys_timerfd_create(clockid: u32, flags: u32) -> SyscallRet {
    if !matches!(clockid, CLOCK_REALTIME | CLOCK_MONOTONIC)
        || flags & !(TFD_NONBLOCK | TFD_CLOEXEC) != 0
    {
        return Err(SysErrNo::EINVAL);
    }

    let nonblocking = flags & TFD_NONBLOCK != 0;
    let mut open_flags = OpenFlags::O_RDONLY;
    if nonblocking {
        open_flags |= OpenFlags::O_NONBLOCK;
    }
    if flags & TFD_CLOEXEC != 0 {
        open_flags |= OpenFlags::O_CLOEXEC;
    }

    let task = current_task().unwrap();
    let fd = task.process.fd_table.alloc_fd()?;
    let timerfd = TimerFd::new(nonblocking);
    task.process.fd_table.set(
        fd,
        FileDescriptor::new(open_flags, FileClass::Abs(timerfd.clone())),
    )?;
    TimerFd::register_fd(fd, &timerfd);
    Ok(fd)
}

/// https://www.man7.org/linux/man-pages/man2/timerfd_settime.2.html
pub fn sys_timerfd_settime(
    fd: u32,
    flags: u32,
    new_value: *const u8,
    old_value: *mut u8,
) -> SyscallRet {
    if flags & !TFD_TIMER_ABSTIME != 0 || new_value.is_null() {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let memory_set = task.process.memory_set_arc();
    let spec: ITimerSpec = copy_from_user_val(&memory_set, new_value.cast())?;
    if spec.it_interval.tv_nsec >= 1_000_000_000 || spec.it_value.tv_nsec >= 1_000_000_000 {
        return Err(SysErrNo::EINVAL);
    }

    let timerfd = TimerFd::lookup(fd as usize)?;
    if !old_value.is_null() {
        let old = timerfd.current_spec();
        copy_to_user_val(
            &memory_set,
            old_value.cast(),
            &ITimerSpec {
                it_interval: old.interval,
                it_value: old.value,
            },
        )?;
    }
    timerfd.set_time(
        flags & TFD_TIMER_ABSTIME != 0,
        TimerFdSpec {
            interval: spec.it_interval,
            value: spec.it_value,
        },
    );
    Ok(0)
}

/// https://www.man7.org/linux/man-pages/man2/timerfd_gettime.2.html
pub fn sys_timerfd_gettime(fd: u32, curr_value: *mut u8) -> SyscallRet {
    if curr_value.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let memory_set = task.process.memory_set_arc();
    let timerfd = TimerFd::lookup(fd as usize)?;
    let spec = timerfd.current_spec();
    copy_to_user_val(
        &memory_set,
        curr_value.cast(),
        &ITimerSpec {
            it_interval: spec.interval,
            it_value: spec.value,
        },
    )?;
    Ok(0)
}
