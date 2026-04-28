use crate::syscall::Syscall;
use crate::task::current_task;
use crate::utils::{SyscallRet, SysErrNo};
use log::{debug, error, warn};

fn dup_fd(old_fd:usize, cloexec: bool) ->SyscallRet {
    let task=current_task().unwrap();
    let proc_inner=task.get_process().inner_lock();
    let file = match proc_inner.fd_table.try_get(old_fd) {
        Some(f)=>f.clone(),
        None=>return Err(SysErrNo::EBADF),
    };
    let new_fd=proc_inner.fd_table.alloc_fd()?;
    let mut new_file=file;
    if !cloexec {
        new_file.unset_cloexec();
    }else{
        new_file.set_cloexec();
    }
    proc_inner.fd_table.set(new_fd, new_file);
    proc_inner.fs_info.dup_fd_path(old_fd, new_fd);
    Ok(new_fd)
}

/// 参考 https://man7.org/linux/man-pages/man2/dup.2.html
pub fn sys_dup(fd: usize) -> SyscallRet {
    debug!("[sys_dup]: fd is {fd}");
    dup_fd(fd, false)
}

/// 参考 https://man7.org/linux/man-pages/man2/dup3.2.html
pub fn sys_dup3(old: usize, new: usize, flags: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();

    debug!(
        "[sys_dup3] : oldfd is {}, newfd is {}, flags is {}",
        old, new, flags
    );
    if old == new {
        return Err(SysErrNo::EINVAL);
    }
    if old >= proc_inner.fd_table.len() || new >= proc_inner.fd_table.get_soft_limit() {
        return Err(SysErrNo::EMFILE); // 添加文件描述符耗尽检查
    }

    if old >= proc_inner.fd_table.len()
        || (old as isize) < 0
        || (new as isize) < 0
        || new >= proc_inner.fd_table.get_soft_limit()
    {
        error!("lots of");
        return Err(SysErrNo::EBADF);
    }
    // 检查文件描述符表是否已满
    if proc_inner.fd_table.try_get(old).is_none() {
        return Err(SysErrNo::EINVAL);
    }

    if proc_inner.fd_table.len() <= new {
        proc_inner.fd_table.resize(new + 1)?;
    }

    let mut file = proc_inner.fd_table.get(old);
    if flags == 0x800000 || flags == 0x80000 {
        //flags包含O_CLOEXEC,为新的fd设置该标志，否则不设置
        file.set_cloexec();
    } else {
        file.unset_cloexec();
    }
    proc_inner.fd_table.set(new, file);
    Ok(new)
}

/// https://man7.org/linux/man-pages/man2/fcntl.2.html
pub fn sys_fcntl(fd: usize, cmd: usize, arg: usize) -> SyscallRet {
    debug!("[sys_fcntl]: fd is {}, cmd is {}, arg is {arg}", fd, cmd);
    match cmd as u32 {

    }
    Err(SysErrNo::EINVAL)
}