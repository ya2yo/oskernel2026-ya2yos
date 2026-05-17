use core::sync::atomic::{AtomicI32, Ordering};

use super::fcntl::*;
use crate::fs::{open, FileDescriptor, OpenFlags};
use crate::mm::translated_str;
use crate::syscall::{options::FcntlCmd, process, Syscall};
use crate::task::current_task;
use crate::utils::{SysErrNo, SyscallRet};
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use log::{debug, error, warn};

fn dup_fd(old_fd: usize, cloexec: bool) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc_inner = task.process.inner_lock();
    let mut new_desc = proc_inner.fd_table.get(old_fd)?;
    if cloexec {
        new_desc.set_cloexec();
    } else {
        new_desc.unset_cloexec();
    }
    let new_fd = proc_inner.fd_table.alloc_fd()?;
    if let Err(e) = proc_inner.fd_table.set(new_fd, new_desc) {
        proc_inner.fd_table.take(new_fd);
        return Err(e);
    }
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

    let mut file = proc_inner.fd_table.get(old)?;
    if flags == 0x800000 || flags == 0x80000 {
        //flags包含O_CLOEXEC,为新的fd设置该标志，否则不设置
        file.set_cloexec();
    } else {
        file.unset_cloexec();
    }
    proc_inner.fd_table.set(new, file);
    Ok(new)
}

/// 参考 https://man7.org/linux/man-pages/man2/fcntl.2.html
pub fn sys_fcntl(fd: usize, cmd: usize, arg: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();

    // debug!("[sys_fcntl] fd is {}, cmd is {}, arg is {}", fd, cmd, arg);

    if fd >= proc_inner.fd_table.len() || (fd as isize) < 0 {
        return Err(SysErrNo::EBADF);
    }

    if proc_inner.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EINVAL);
    }

    let mut file = proc_inner.fd_table.get(fd)?;
    let cmd = FcntlCmd::from_bits(cmd).unwrap();

    match cmd {
        FcntlCmd::F_DUPFD => {
            let fd_new = proc_inner.fd_table.alloc_fd_larger_than(arg)?;
            proc_inner.fd_table.set(fd_new, file);
            proc_inner.fs_info.dup_fd_path(fd, fd_new);
            return Ok(fd_new);
        }
        FcntlCmd::F_DUPFD_CLOEXEC => {
            let fd_new = proc_inner.fd_table.alloc_fd_larger_than(arg)?;
            file.set_cloexec();
            proc_inner.fd_table.set(fd_new, file);
            proc_inner.fs_info.dup_fd_path(fd, fd_new);
            return Ok(fd_new);
        }
        FcntlCmd::F_GETFD => {
            return if proc_inner.fd_table.get(fd)?.cloexec() {
                Ok(1)
            } else {
                Ok(0)
            };
        }
        FcntlCmd::F_SETFD => {
            if arg & FD_CLOEXEC as usize == 0 {
                proc_inner.fd_table.unset_cloexec(fd);
            } else {
                proc_inner.fd_table.set_cloexec(fd);
            }
        }
        FcntlCmd::F_GETFL => {
            let mut res = OpenFlags::O_RDWR.bits() as usize;
            if file.non_block() {
                res |= OpenFlags::O_NONBLOCK.bits() as usize;
            }
            return Ok(res);
        }
        FcntlCmd::F_SETFL => {
            // 目前只启用nonblock
            let flags = OpenFlags::from_bits_truncate(arg as u32);
            if flags.contains(OpenFlags::O_NONBLOCK) {
                proc_inner.fd_table.set_nonblock(fd);
            } else {
                proc_inner.fd_table.unset_nonblock(fd);
            }
            // task_inner.fd_table.set_flags(fd, Some(flags));
            // todo!()
        }
        _ => {
            return Err(SysErrNo::EINVAL);
        }
    }
    Ok(0)
}

static TMP_FILE_COUNTER: AtomicI32 = AtomicI32::new(0);

/// 参考 https://man7.org/linux/man-pages/man2/openat.2.html
pub fn sys_openat(dirfd: isize, path: *const u8, flags: u32, mode: u32) -> SyscallRet {
    if path as usize == 0 {
        return Err(SysErrNo::ENOENT);
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let token = proc_inner.get_locked_memory_set_read().token();
    let path = translated_str(token, path);

    let mut flags = OpenFlags::from_bits(flags).unwrap();

    let mut abs_path = proc_inner.get_abs_path(dirfd, &path)?;
    debug!(
        "[sys_openat] path is {}, flags is {:?}, mode is {:o}",
        &abs_path, flags, mode
    );

    if flags.contains(OpenFlags::O_TMPFILE) {
        // 当出现O_TMPFILE时，openat的含意是，
        // dirfd+path指向的位置(abs_path)处是一个文件夹，
        // 在此文件夹处创建一个匿名临时文件并返回临时文件
        assert!(flags.contains(OpenFlags::O_DIRECTORY));
        // 这里我们简化处理一下……我们创建一个真实文件，且永远不会删除它
        flags.insert(OpenFlags::O_CREATE);
        abs_path = {
            let count = TMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
            format!("{}/{}.tmp", abs_path, count)
        };
        flags.remove(OpenFlags::O_TMPFILE); // 因为简化处理，TMP标志位在这里被拦截
        flags.remove(OpenFlags::O_DIRECTORY); // 避免下面的open真的给咱创建一个目录

        // 剩下的就和普通open一样处理就行了
        let inode = open(&abs_path, flags, mode)?;
        let new_fd = proc_inner.fd_table.alloc_fd()?;
        proc_inner
            .fd_table
            .set(new_fd, FileDescriptor::new(flags, inode));
        proc_inner.fs_info.insert(abs_path, new_fd);
        return Ok(new_fd);
    }

    if abs_path == "/proc/self/stat" {
        abs_path = format!("/proc/{}/stat", task.pid());
    }

    let inode = open(&abs_path, flags, mode)?;
    let new_fd = proc_inner.fd_table.alloc_fd()?;
    proc_inner
        .fd_table
        .set(new_fd, FileDescriptor::new(flags, inode));

    proc_inner.fs_info.insert(abs_path, new_fd);
    return Ok(new_fd);
}

/// 参考 https://man7.org/linux/man-pages/man2/close.2.html
pub fn sys_close(fd: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.process.inner_lock(); // 拿到锁就不用调用get_fd_table了
    let fd_table = inner.fd_table.clone();
    debug!("[sys_close] fd is {}", fd);

    if (fd as isize) < 0 || fd >= fd_table.len() {
        return Err(SysErrNo::EBADF);
    }

    if fd_table.try_get(fd).is_none() {
        return Ok(0);
    }

    fd_table.take(fd);
    inner.fs_info.remove(fd);
    Ok(0)
}
