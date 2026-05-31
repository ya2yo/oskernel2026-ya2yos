use alloc::string::ToString;
use log::debug;

use crate::{
    fs::{make_pipe, FileClass, FileDescriptor},
    mm::translated_refmut,
    task::current_task,
    utils::SyscallRet,
};

/// 参考 https://man7.org/linux/man-pages/man2/pipe2.2.html
pub fn sys_pipe2(fd: *mut u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let fd_table = proc_inner.fd_table.clone();
    let token = proc_inner.get_locked_memory_set_write().token();

    let (read_pipe, write_pipe) = make_pipe();
    let read_fd = fd_table.alloc_fd()?;
    proc_inner
        .fd_table
        .set(read_fd, FileDescriptor::default(FileClass::Abs(read_pipe)));
    let write_fd = fd_table.alloc_fd()?;
    fd_table.set(
        write_fd,
        FileDescriptor::default(FileClass::Abs(write_pipe)),
    );
    let locked_fs_info = proc_inner.fs_info.clone();

    locked_fs_info.insert("pipe".to_string(), read_fd);
    locked_fs_info.insert("pipe".to_string(), write_fd);
    // debug!("pipe read fd is {}, write fd is {}", read_fd, write_fd);
    *translated_refmut(token, fd) = read_fd as u32;
    *translated_refmut(token, unsafe { fd.add(1) }) = write_fd as u32;
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
