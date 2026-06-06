//! `epoll_create1` / `epoll_ctl` / `epoll_pwait` — 参数与用户缓冲区，语义在 `fs::files::epoll`。

use alloc::sync::Arc;
use log::debug;

use linux_raw_sys::general::{epoll_event, EPOLL_CTL_ADD, EPOLL_CTL_DEL, EPOLL_CTL_MOD};

use crate::{
    fs::{EpollCreateFlags, EpollFile, FileClass, FileDescriptor, OpenFlags},
    mm::{copy_from_user, copy_to_user},
    task::{current_task, suspend_current_and_run_next},
    timer::get_time_ms,
    utils::{SysErrNo, SyscallRet},
};

/// https://www.man7.org/linux/man-pages/man2/epoll_create.2.html
pub fn sys_epoll_create1(flags: u32) -> SyscallRet {
    let eflags = EpollCreateFlags::from_bits(flags).ok_or(SysErrNo::EINVAL)?;
    debug!("[sys_epoll_create1] flags={:?}", eflags);

    let task = current_task().unwrap();
    let epoll_file = Arc::new(EpollFile::new());
    let proc_inner = task.process.inner_lock();

    let open_flags = if eflags.contains(EpollCreateFlags::CLOEXEC) {
        OpenFlags::O_CLOEXEC
    } else {
        OpenFlags::empty()
    };

    let fd = proc_inner.fd_table.alloc_fd()?;
    proc_inner.fd_table.set(
        fd,
        FileDescriptor::new(open_flags, FileClass::Abs(epoll_file.clone())),
    )?;

    EpollFile::register_fd(fd, &epoll_file);

    debug!("[sys_epoll_create1] created epoll fd={}", fd);
    Ok(fd)
}

/// https://www.man7.org/linux/man-pages/man2/epoll_ctl.2.html
pub fn sys_epoll_ctl(epfd: usize, op: usize, fd: usize, event_ptr: usize) -> SyscallRet {
    debug!("[sys_epoll_ctl] epfd={}, op={}, fd={}, event_ptr={}", epfd, op, fd, event_ptr);
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let fd_table = &process.fd_table;
    let fd_i32 = fd as i32;
    let epoll_file = EpollFile::lookup(epfd)?;

    fd_table.get(fd)?;
    if fd == epfd {
        return Err(SysErrNo::EINVAL);
    }

    match op {
        x if x == EPOLL_CTL_ADD as usize => {
            // Per Linux semantics, if the fd is already in the epoll set,
            // return EEXIST without accessing the event pointer.
            if epoll_file.contains_fd(fd_i32) {
                return Err(SysErrNo::EEXIST);
            }
            let mut event: epoll_event = unsafe { core::mem::zeroed() };
            copy_from_user(&memory_set, event_ptr, unsafe {
                core::slice::from_raw_parts_mut(&mut event as *mut epoll_event as *mut u8, core::mem::size_of::<epoll_event>())
            })?;
            debug!(
                "[sys_epoll_ctl] ADD epfd={}, fd={}, events=0x{:x}, data=0x{:x}",
                epfd, fd, event.events, event.data
            );
            epoll_file.ctl_add(fd_i32, event.events, event.data)?;
            Ok(0)
        }
        x if x == EPOLL_CTL_MOD as usize => {
            // Per Linux semantics, if the fd is not in the epoll set,
            // return ENOENT without accessing the event pointer.
            if !epoll_file.contains_fd(fd_i32) {
                return Err(SysErrNo::ENOENT);
            }
            let mut event: epoll_event = unsafe { core::mem::zeroed() };
            copy_from_user(&memory_set, event_ptr, unsafe {
                core::slice::from_raw_parts_mut(&mut event as *mut epoll_event as *mut u8, core::mem::size_of::<epoll_event>())
            })?;
            debug!(
                "[sys_epoll_ctl] MOD epfd={}, fd={}, events=0x{:x}, data=0x{:x}",
                epfd, fd, event.events, event.data
            );
            epoll_file.ctl_mod(fd_i32, event.events, event.data)?;
            Ok(0)
        }
        x if x == EPOLL_CTL_DEL as usize => {
            debug!("[sys_epoll_ctl] DEL epfd={}, fd={}", epfd, fd);
            epoll_file.ctl_del(fd_i32)?;
            Ok(0)
        }
        _ => Err(SysErrNo::EINVAL),
    }
}

/// https://www.man7.org/linux/man-pages/man2/epoll_wait.2.html
pub fn sys_epoll_pwait(
    epfd: usize,
    events_ptr: usize,
    maxevents: usize,
    timeout: usize,
    _sigmask: usize,
) -> SyscallRet {
    if events_ptr == 0 && maxevents > 0 {
        return Err(SysErrNo::EFAULT);
    }
    if maxevents == 0 {
        return Ok(0);
    }

    // If the epoll set is empty, return immediately to avoid busy-wait.
    // Linux would block until a signal arrives, but we don't have that
    // wakeup mechanism yet.
    {
        let epoll_file = EpollFile::lookup(epfd)?;
        if epoll_file.is_interests_empty() {
            return Ok(0);
        }
    }

    let waittime: isize = if timeout == usize::MAX {
        -1
    } else {
        timeout as isize
    };

    if waittime == 0 {
        return epoll_wait_once(epfd, events_ptr, maxevents);
    }

    let begin = get_time_ms();
    loop {
        let count = epoll_wait_once(epfd, events_ptr, maxevents)?;
        if count > 0 {
            return Ok(count);
        }
        if waittime > 0 && (get_time_ms() - begin) >= waittime as usize {
            return Ok(0);
        }
        suspend_current_and_run_next();
    }
}

/// 单次扫描：写用户 `epoll_event` 数组并返回就绪数量。
fn epoll_wait_once(epfd: usize, events_ptr: usize, maxevents: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let epoll_file = EpollFile::lookup(epfd)?;
    let fd_table = &process.fd_table;
    let mut poll_one = |fd: i32, registered: u32| {
        let desc = fd_table.try_get(fd as usize)?;
        let file = desc.any();
        Some(EpollFile::poll_mask(file.as_ref(), registered))
    };

    let ready = epoll_file.collect_ready(&mut poll_one, maxevents);
    for (i, ev) in ready.iter().enumerate() {
        let user_event = epoll_event {
            events: ev.events,
            data: ev.data,
        };
        copy_to_user(&memory_set, events_ptr + i * core::mem::size_of::<epoll_event>(), unsafe {
            core::slice::from_raw_parts(&user_event as *const epoll_event as *const u8, core::mem::size_of::<epoll_event>())
        })?;
    }
    Ok(ready.len())
}
