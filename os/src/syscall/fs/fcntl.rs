//! linux-7.0/include/uapi/asm-generic/fcntl.h
//! linux-7.0/include/uapi/linux/fcntl.h
use core::{future::poll_fn, task::Poll};

use super::file_lock::{self, Flock};
use crate::{
    fs::{File, OpenFlags, SEEK_CUR as FS_SEEK_CUR},
    mm::{copy_from_user, copy_to_user},
    syscall::options::FcntlCmd,
    task::{block_on, current_task, interruptible},
    utils::{SysErrNo, SyscallRet},
};
use alloc::string::String;

pub const F_DUPFD: u32 = 0; /* dup */
pub const F_GETFD: u32 = 1; /* get close_on_exec */
pub const F_SETFD: u32 = 2; /* set/clear close_on_exec */
pub const F_GETFL: u32 = 3; /* get file->f_flags */
pub const F_SETFL: u32 = 4; /* set file->f_flags */
pub const F_GETLK: u32 = 5;
pub const F_SETLK: u32 = 6;
pub const F_SETLKW: u32 = 7;
pub const F_SETOWN: u32 = 8; /* for sockets. */
pub const F_GETOWN: u32 = 9; /* for sockets. */
pub const F_SETSIG: u32 = 10; /* for sockets. */
pub const F_GETSIG: u32 = 11; /* for sockets. */
pub const F_GETLK64: u32 = 12; /* using 'struct flock64' */
pub const F_SETLK64: u32 = 13;
pub const F_SETLKW64: u32 = 14;
pub const F_SETOWN_EX: u32 = 15;
pub const F_GETOWN_EX: u32 = 16;
pub const F_OFD_GETLK: u32 = 36;
pub const F_OFD_SETLK: u32 = 37;
pub const F_OFD_SETLKW: u32 = 38;
pub const F_SETLEASE: u32 = 1024;
pub const F_GETLEASE: u32 = 1025;
pub const F_NOTIFY: u32 = 1026;
pub const F_DUPFD_QUERY: u32 = 1027;
pub const F_DUPFD_CLOEXEC: u32 = 1030;
pub const F_SETPIPE_SZ: u32 = 1031;
pub const F_GETPIPE_SZ: u32 = 1032;

/* for F_[GET|SET]FL */
pub const FD_CLOEXEC: u32 = 1; /* actually anything with low bit set goes */

/* for F_[GET|SET]LK */
pub const F_RDLCK: i16 = 0;
pub const F_WRLCK: i16 = 1;
pub const F_UNLCK: i16 = 2;

/* for F_SETOWN_EX */
pub const F_OWNER_TID: i32 = 0;
pub const F_OWNER_PID: i32 = 1;
pub const F_OWNER_PGRP: i32 = 2;

/* for F_NOTIFY */
pub const DN_ACCESS: u32 = 0x00000001;
pub const DN_MODIFY: u32 = 0x00000002;
pub const DN_CREATE: u32 = 0x00000004;
pub const DN_DELETE: u32 = 0x00000008;
pub const DN_RENAME: u32 = 0x00000010;
pub const DN_ATTRIB: u32 = 0x00000020;
pub const DN_MULTISHOT: u32 = 0x80000000;

/* for flock() */
pub const LOCK_SH: i32 = 1;
pub const LOCK_EX: i32 = 2;
pub const LOCK_NB: i32 = 4;
pub const LOCK_UN: i32 = 8;

pub const SEEK_SET: i16 = 0;
pub const SEEK_CUR: i16 = 1;
pub const SEEK_END: i16 = 2;

#[derive(Debug, Clone, Copy)]
struct FOwnerEx {
    owner_type: i32,
    pid: i32,
}

impl FOwnerEx {
    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }
        Some(Self {
            owner_type: i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            pid: i32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }

    fn to_bytes(self) -> [u8; 8] {
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&self.owner_type.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.pid.to_ne_bytes());
        bytes
    }
}

fn setlk_blocking(
    path: String,
    flock: Flock,
    file_size: i64,
    current_offset: i64,
    owner_pid: i32,
) -> SyscallRet {
    let result = block_on(interruptible(poll_fn(move |cx| {
        match file_lock::setlk(&path, &flock, file_size, current_offset, owner_pid) {
            Ok(ret) => {
                file_lock::clear_wait(owner_pid);
                Poll::Ready(Ok(ret))
            }
            Err(SysErrNo::EAGAIN) => {
                let owners = file_lock::conflicting_owners(
                    &path,
                    &flock,
                    file_size,
                    current_offset,
                    owner_pid,
                );
                if file_lock::would_deadlock(owner_pid, &owners) {
                    file_lock::clear_wait(owner_pid);
                    return Poll::Ready(Err(SysErrNo::EDEADLK));
                }
                file_lock::record_wait(owner_pid, &owners);
                file_lock::register_posix_waker(&path, cx.waker());
                match file_lock::setlk(&path, &flock, file_size, current_offset, owner_pid) {
                    Ok(ret) => {
                        file_lock::clear_wait(owner_pid);
                        Poll::Ready(Ok(ret))
                    }
                    Err(SysErrNo::EAGAIN) => Poll::Pending,
                    Err(e) => {
                        file_lock::clear_wait(owner_pid);
                        Poll::Ready(Err(e))
                    }
                }
            }
            Err(e) => {
                file_lock::clear_wait(owner_pid);
                Poll::Ready(Err(e))
            }
        }
    })));
    file_lock::clear_wait(owner_pid);
    match result {
        Ok(ret) => ret,
        Err(err) => Err(err.into()),
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/fcntl.2.html
pub fn sys_fcntl(fd: usize, cmd: usize, arg: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let owner_pid = task.pid() as i32;

    // debug!("[sys_fcntl] fd is {}, cmd is {}, arg is {}", fd, cmd, arg);

    let fd_desc = proc_inner.fd_table.get(fd)?;
    let cmd = FcntlCmd::from_bits(cmd).ok_or(SysErrNo::EINVAL)?;

    match cmd {
        FcntlCmd::F_DUPFD => {
            let mut file = proc_inner.fd_table.get(fd)?;
            file.unset_cloexec();
            let fd_new = proc_inner.fd_table.alloc_fd_larger_than(arg)?;
            proc_inner.fd_table.set(fd_new, file);
            proc_inner.fs_info.dup_fd_path(fd, fd_new);
            return Ok(fd_new);
        }
        FcntlCmd::F_DUPFD_CLOEXEC => {
            let mut file = proc_inner.fd_table.get(fd)?;
            file.set_cloexec();
            let fd_new = proc_inner.fd_table.alloc_fd_larger_than(arg)?;
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
            let file = proc_inner.fd_table.get(fd)?;
            return Ok(file.getfl_flags() as usize);
        }
        FcntlCmd::F_SETFL => {
            let file = proc_inner.fd_table.get(fd)?;
            let flags = OpenFlags::from_bits_truncate(arg as u32);
            proc_inner.fd_table.set_status_flags(fd, flags)?;
            if flags.contains(OpenFlags::O_NONBLOCK) {
                file.any().set_nonblocking(true)?;
            } else {
                file.any().set_nonblocking(false)?;
            }
        }
        // 文件记录锁（F_GETLK / F_SETLK / F_SETLKW）
        // 按 inode 路径在全局锁表中管理 POSIX advisory record lock
        FcntlCmd::F_GETLK => {
            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let mut flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            let (inode_path, file_size, current_offset) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (
                    osfile.inode.path(),
                    osfile.inode.size() as i64,
                    osfile.lseek(0, FS_SEEK_CUR)? as i64,
                )
            };

            file_lock::getlk(
                &inode_path,
                &mut flock,
                file_size,
                current_offset,
                owner_pid,
            )?;

            let result_bytes = flock.to_bytes();
            copy_to_user(&memory_set, arg, &result_bytes)?;
            return Ok(0);
        }
        FcntlCmd::F_SETLK => {
            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            let (inode_path, file_size, current_offset) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (
                    osfile.inode.path(),
                    osfile.inode.size() as i64,
                    osfile.lseek(0, FS_SEEK_CUR)? as i64,
                )
            };

            file_lock::setlk(&inode_path, &flock, file_size, current_offset, owner_pid)?;
            return Ok(0);
        }
        FcntlCmd::F_SETLKW => {
            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            let (inode_path, file_size, current_offset) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (
                    osfile.inode.path(),
                    osfile.inode.size() as i64,
                    osfile.lseek(0, FS_SEEK_CUR)? as i64,
                )
            };

            return setlk_blocking(inode_path, flock, file_size, current_offset, owner_pid);
        }
        // OFD（Open File Description）锁 — 简化委托给 POSIX 锁逻辑
        FcntlCmd::F_OFD_GETLK => {
            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let mut flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            let (inode_path, file_size, current_offset, ofd_owner) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (
                    osfile.inode.path(),
                    osfile.inode.size() as i64,
                    osfile.lseek(0, FS_SEEK_CUR)? as i64,
                    osfile.ofd_lock_owner(),
                )
            };

            file_lock::getlk(
                &inode_path,
                &mut flock,
                file_size,
                current_offset,
                ofd_owner,
            )?;
            if flock.l_type != F_UNLCK {
                flock.l_pid = -1;
            }

            let result_bytes = flock.to_bytes();
            copy_to_user(&memory_set, arg, &result_bytes)?;
            return Ok(0);
        }
        FcntlCmd::F_OFD_SETLK => {
            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            let (inode_path, file_size, current_offset, ofd_owner) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (
                    osfile.inode.path(),
                    osfile.inode.size() as i64,
                    osfile.lseek(0, FS_SEEK_CUR)? as i64,
                    osfile.ofd_lock_owner(),
                )
            };

            file_lock::setlk(&inode_path, &flock, file_size, current_offset, ofd_owner)?;
            return Ok(0);
        }
        FcntlCmd::F_OFD_SETLKW => {
            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            let (inode_path, file_size, current_offset, ofd_owner) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (
                    osfile.inode.path(),
                    osfile.inode.size() as i64,
                    osfile.lseek(0, FS_SEEK_CUR)? as i64,
                    osfile.ofd_lock_owner(),
                )
            };

            return setlk_blocking(inode_path, flock, file_size, current_offset, ofd_owner);
        }
        // 文件 owner / 信号（主要用于套接字）
        FcntlCmd::F_GETOWN => {
            let owner = fd_desc.any().fasync_owner();
            let pid = if owner.owner_type == F_OWNER_PGRP {
                -owner.pid
            } else {
                owner.pid
            };
            return Ok(pid as usize);
        }
        FcntlCmd::F_SETOWN => {
            let mut owner = fd_desc.any().fasync_owner();
            let pid = arg as isize as i32;
            if pid < 0 {
                owner.owner_type = F_OWNER_PGRP;
                owner.pid = pid.saturating_neg();
            } else {
                owner.owner_type = F_OWNER_PID;
                owner.pid = pid;
            }
            fd_desc.any().set_fasync_owner(owner)?;
        }
        FcntlCmd::F_SETSIG => {
            let signal = arg as i32;
            if signal < 0 || signal as usize > crate::signal::SIG_MAX_NUM {
                return Err(SysErrNo::EINVAL);
            }
            let mut owner = fd_desc.any().fasync_owner();
            owner.signal = signal;
            fd_desc.any().set_fasync_owner(owner)?;
        }
        FcntlCmd::F_GETSIG => {
            return Ok(fd_desc.any().fasync_owner().signal as usize);
        }
        FcntlCmd::F_SETOWN_EX => {
            let memory_set = proc_inner.memory_set_arc();
            let mut owner_bytes = [0u8; 8];
            copy_from_user(&memory_set, arg, &mut owner_bytes)?;
            let owner_ex = FOwnerEx::from_bytes(&owner_bytes).ok_or(SysErrNo::EINVAL)?;
            if owner_ex.owner_type != F_OWNER_TID
                && owner_ex.owner_type != F_OWNER_PID
                && owner_ex.owner_type != F_OWNER_PGRP
            {
                return Err(SysErrNo::EINVAL);
            }
            if owner_ex.pid < 0 {
                return Err(SysErrNo::EINVAL);
            }
            let mut owner = fd_desc.any().fasync_owner();
            owner.owner_type = owner_ex.owner_type;
            owner.pid = owner_ex.pid;
            fd_desc.any().set_fasync_owner(owner)?;
        }
        FcntlCmd::F_GETOWN_EX => {
            let memory_set = proc_inner.memory_set_arc();
            let owner = fd_desc.any().fasync_owner();
            let owner_ex = FOwnerEx {
                owner_type: owner.owner_type,
                pid: owner.pid,
            };
            copy_to_user(&memory_set, arg, &owner_ex.to_bytes())?;
            return Ok(0);
        }
        // 文件租约（file lease）-
        FcntlCmd::F_SETLEASE => {
            let osfile = fd_desc.file()?;
            let flags = OpenFlags::from_bits_truncate(fd_desc.flags());
            let (_, fd_opened_for_write) = flags.read_write();
            return file_lock::set_file_lease(
                &osfile.inode.path(),
                arg as i16,
                owner_pid,
                fd_opened_for_write,
            );
        }
        FcntlCmd::F_GETLEASE => {
            let osfile = fd_desc.file()?;
            return Ok(file_lock::get_file_lease(&osfile.inode.path(), owner_pid) as usize);
        }
        // 目录变动通知
        FcntlCmd::F_NOTIFY => {
            return Err(SysErrNo::EINVAL);
        }
        // DUPFD_QUERY — 查询 F_DUPFD 将分配的 fd 编号（不实际分配）
        FcntlCmd::F_DUPFD_QUERY => {
            let fd_table = &proc_inner.fd_table;
            let soft_limit = fd_table.get_soft_limit();
            if arg >= soft_limit {
                return Err(SysErrNo::EINVAL);
            }
            for candidate in arg..soft_limit {
                if fd_table.try_get(candidate).is_none() {
                    return Ok(candidate);
                }
            }
            return Err(SysErrNo::EMFILE);
        }
        // pipe 大小
        FcntlCmd::F_SETPIPE_SZ => {
            let pipe = proc_inner.fd_table.get(fd)?.pipe()?;
            if arg > (1usize << 31) {
                return Err(SysErrNo::EINVAL);
            }
            return Ok(pipe.set_capacity(arg)?);
        }
        FcntlCmd::F_GETPIPE_SZ => {
            let pipe = proc_inner.fd_table.get(fd)?.pipe()?;
            return Ok(pipe.capacity());
        }

        _ => {
            return Err(SysErrNo::EINVAL);
        }
    }
    Ok(0)
}
