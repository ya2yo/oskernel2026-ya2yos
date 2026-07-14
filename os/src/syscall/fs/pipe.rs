use alloc::string::ToString;
use log::{debug, warn};

use super::dummyfd_create;
use crate::{
    fs::{make_pipe, File, FileClass, FileDescriptor, OpenFlags},
    mm::copy_to_user,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/pipe2.2.html
pub fn sys_pipe2(fd: *mut u32, flags: u32) -> SyscallRet {
    let allowed_flags = OpenFlags::O_CLOEXEC | OpenFlags::O_NONBLOCK | OpenFlags::O_DIRECT;
    if flags & !allowed_flags.bits() != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let flags = OpenFlags::from_bits(flags).ok_or(SysErrNo::EINVAL)?;

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let fd_table = proc_inner.fd_table.clone();
    let memory_set = proc_inner.memory_set_arc();

    let (read_pipe, write_pipe) = make_pipe();
    if flags.contains(OpenFlags::O_NONBLOCK) {
        read_pipe.set_nonblocking(true)?;
        write_pipe.set_nonblocking(true)?;
    }

    let read_flags = flags & (OpenFlags::O_CLOEXEC | OpenFlags::O_NONBLOCK);
    // Linux exposes O_DIRECT only on the write end of a pipe created with pipe2().
    let write_flags = read_flags | (flags & OpenFlags::O_DIRECT);
    let read_fd = fd_table.alloc_fd()?;
    fd_table.set(
        read_fd,
        FileDescriptor::new(read_flags, FileClass::Pipe(read_pipe)),
    )?;
    let write_fd = fd_table.alloc_fd()?;
    fd_table.set(
        write_fd,
        FileDescriptor::new(write_flags, FileClass::Pipe(write_pipe)),
    )?;
    let locked_fs_info = proc_inner.fs_info.clone();

    locked_fs_info.insert("pipe".to_string(), read_fd);
    locked_fs_info.insert("pipe".to_string(), write_fd);
    // debug!("pipe read fd is {}, write fd is {}", read_fd, write_fd);
    let rfd = read_fd as u32;
    copy_to_user(&memory_set, fd as usize, unsafe {
        core::slice::from_raw_parts(&rfd as *const u32 as *const u8, core::mem::size_of::<u32>())
    })?;
    let wfd = write_fd as u32;
    copy_to_user(&memory_set, unsafe { fd.add(1) } as usize, unsafe {
        core::slice::from_raw_parts(&wfd as *const u32 as *const u8, core::mem::size_of::<u32>())
    })?;
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/pidfd_open.2.html
pub fn sys_pidfd_open(_pid: u32, _flags: u32) -> SyscallRet {
    warn!("[sys_pidfd_open] not implement!");
    dummyfd_create()
}
/// https://man7.org/linux/man-pages/man2/pidfd_getfd.2.html
pub fn sys_pidfd_getfd(_pidfd: i32, _target_fd: i32, _flags: u32) -> SyscallRet {
    warn!("[sys_pidfd_getfd] not implement!");
    Ok(0)
}
