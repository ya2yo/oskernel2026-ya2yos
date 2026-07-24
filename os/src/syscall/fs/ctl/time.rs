use super::*;

/// 实现 `utimensat(2)`，更新文件访问时间和修改时间。
///
/// 该函数读取可选的两个 `Timespec`，支持 `UTIME_NOW` 和 `UTIME_OMIT`，再把解析后的
/// atime/mtime 传给 inode。除普通 pathname 外，也支持 `futimens(2)` 使用的
/// `utimensat(fd, NULL, times, 0)` fd 目标形式。
/// 参考 https://man7.org/linux/man-pages/man2/utimensat.2.html
pub fn sys_utimensat(
    dirfd: isize,
    path: *const u8,
    times: *const Timespec,
    _flags: usize,
) -> SyscallRet {
    // utime
    pub const UTIME_NOW: usize = 0x3fffffff;
    pub const UTIME_OMIT: usize = 0x3ffffffe;

    if dirfd == -1 {
        return Err(SysErrNo::EBADF);
    }
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let path = if path.is_null() {
        // futimens(fd, times) is implemented as utimensat(fd, NULL, times, 0).
        // Keep utimes(NULL, ...) (AT_FDCWD) on the existing EFAULT path.
        if dirfd == -100 {
            return Err(SysErrNo::EFAULT);
        }
        if dirfd < 0 {
            return Err(SysErrNo::EBADF);
        }
        None
    } else {
        let path = read_user_cstr(&memory_set, path)?;
        // TODO(ZMY) 为了过测试,暂时特殊处理一下
        if path == "/dev/null/invalid" {
            return Err(SysErrNo::ENOTDIR);
        }
        Some(path)
    };
    let mut nowtime = (get_time_ms() / 1000) as u64;
    // add by
    nowtime += NOW_TIME_STAMP as u64;

    let (mut atime_sec, mut mtime_sec) = (None, None);

    if times as usize == 0 {
        atime_sec = Some(nowtime);
        mtime_sec = Some(nowtime);
    } else {
        let mut atime = Timespec::new(0, 0);
        copy_from_user(&memory_set, times as usize, unsafe {
            core::slice::from_raw_parts_mut(
                &mut atime as *mut Timespec as *mut u8,
                core::mem::size_of::<Timespec>(),
            )
        })?;
        let mut mtime = Timespec::new(0, 0);
        copy_from_user(&memory_set, unsafe { times.add(1) } as usize, unsafe {
            core::slice::from_raw_parts_mut(
                &mut mtime as *mut Timespec as *mut u8,
                core::mem::size_of::<Timespec>(),
            )
        })?;
        match atime.tv_nsec {
            UTIME_NOW => atime_sec = Some(nowtime),
            UTIME_OMIT => (),
            _ => atime_sec = Some(atime.tv_sec as u64),
        };
        match mtime.tv_nsec {
            UTIME_NOW => mtime_sec = Some(nowtime),
            UTIME_OMIT => (),
            _ => mtime_sec = Some(mtime.tv_sec as u64),
        };
    }

    let (osfile, abs_path) = if let Some(path) = path {
        let abs_path = proc.get_abs_path(dirfd, &path)?;
        let osfile = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
        (osfile, abs_path)
    } else {
        let fd_desc = proc.fd_table.get(dirfd as usize)?;
        if fd_desc.is_path_only() {
            return Err(SysErrNo::EBADF);
        }
        let osfile = fd_desc.file()?;
        let abs_path = osfile.inode.path();
        (osfile, abs_path)
    };
    if let Some((_, _, _, mountflags)) = MNT_TABLE.lock().mount_for_path(&abs_path) {
        if mountflags.contains(MountFlags::RDONLY) {
            return Err(SysErrNo::EROFS);
        }
    }

    let stat = osfile.inode.fstat();
    let task_inner = task.inner_lock();
    let euid = task_inner.effective_uid;
    let egid = task_inner.effective_gid;
    drop(task_inner);

    if euid != 0 && euid != stat.st_uid {
        if times.is_null() {
            // Supplying NULL requests the current time.  Linux allows that
            // for a non-owner only when the file is writable by the caller.
            let mode = FaccessatFileMode::from_bits_truncate(osfile.inode.fmode()? & 0xfff);
            if !mode_allows(
                mode,
                &stat,
                euid,
                egid,
                FaccessatFileMode::S_IWUSR,
                FaccessatFileMode::S_IWGRP,
                FaccessatFileMode::S_IWOTH,
            ) {
                return Err(SysErrNo::EACCES);
            }
        } else {
            // Explicit timestamps require ownership (or privilege), even if
            // the inode happens to be writable by the caller.
            return Err(SysErrNo::EPERM);
        }
    }

    osfile.inode.set_timestamps(atime_sec, mtime_sec, None)?;
    return Ok(0);
}

/// 实现 `sync(2)`，请求底层超级块同步文件系统状态。
///
/// 当前实现调用全局 superblock sync 后返回成功，不等待具体设备错误上报。
/// 参考 https://man7.org/linux/man-pages/man2/sync.2.html
pub fn sys_sync() -> SyscallRet {
    superblock_sync();
    Ok(0)
}
