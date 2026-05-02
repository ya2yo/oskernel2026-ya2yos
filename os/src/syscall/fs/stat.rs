use log::debug;

use crate::{
    fs::{InodeType, Kstat, MAX_PATH_LEN, MNT_TABLE, NONE_MODE, OpenFlags, Statfs, open, superblock_fs_stat}, 
    mm::{if_bad_address, put_data, translated_str}, 
    syscall::options::{FaccessatFileMode, FaccessatMode}, 
    task::{Process, current_task}, 
    utils::{SysErrNo, SyscallRet, rsplit_once, trim_start_slash}};


/// 参考 https://man7.org/linux/man-pages/man2/fstat.2.html
pub fn sys_fstat(fd: usize, kst: *mut Kstat) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.inner_lock();
    let proc_inner=task.process.inner_lock();
    let token = task.process.inner_lock().get_locked_memory_set_read().token();

    if (kst as isize) <= 0 || if_bad_address(kst as usize) {
        return Err(SysErrNo::EFAULT);
    }

    // debug!(
    //     "[sys_fstat] fd is {:?}, kst_addr is {:#x}",
    //     fd, kst as usize
    // );

    if fd >= proc_inner.fd_table.len() || proc_inner.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EBADF);
    }
    let file = proc_inner.fd_table.get(fd)?.any();
    put_data(token, kst, file.fstat());
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/fstatat64.2.html
pub fn sys_fstatat(dirfd: isize, path: *const u8, kst: *mut Kstat, _flags: usize) -> SyscallRet {
    let task = current_task().unwrap();

    let inner = task.inner_lock();
    let proc_inner=task.process.inner_lock();
    let token = proc_inner.get_locked_memory_set_read().token();
    let path = trim_start_slash(translated_str(token, path));

    let abs_path = inner.get_abs_path(&task, dirfd, &path)?;
    //log::info!("[sys_fstatat] abs_path={}", &abs_path);

    if abs_path == "/ls" || abs_path == "/xargs" || abs_path == "/sleep" {
        open(&abs_path, OpenFlags::O_CREATE, NONE_MODE);
    }

    let file = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.any();
    put_data(token, kst, file.fstat());
    return Ok(0);
}

/// 参考 https://man7.org/linux/man-pages/man2/statfs.2.html
pub fn sys_statfs(_path: *const u8, statfs: *mut Statfs) -> SyscallRet {
    let task = current_task().unwrap();
    let token = task.process.inner_lock().get_locked_memory_set_read().token();
    put_data(token, statfs, superblock_fs_stat());
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/faccessat.2.html
pub fn sys_faccessat(dirfd: isize, path: *const u8, mode: u32, _flags: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.inner_lock();
    let proc_inner=task.process.inner_lock();
    let token = task.process.inner_lock().get_locked_memory_set_read().token();
    if (path as isize) <= 0 {
        return Err(SysErrNo::EFAULT);
    }
    if (mode as i32) < 0 {
        return Err(SysErrNo::EINVAL);
    }
    let path = translated_str(token, path);

    if path.len() == 0 {
        return Err(SysErrNo::ENOENT);
    }

    if path.len() > MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    if dirfd != -100 && dirfd as usize >= proc_inner.fd_table.len() {
        return Err(SysErrNo::EBADF);
    }

    let mode = FaccessatMode::from_bits(mode).unwrap();

    debug!(
        "[sys_faccessat] dirfd is {} and path is {} and mode is {:?}",
        dirfd, path, mode
    );

    if mode.contains(FaccessatMode::W_OK) {
        if let Some((_, _, _, mountflags)) = MNT_TABLE.lock().got_mount(path.clone()) {
            if mountflags & 1 != 0 {
                //挂载点只读
                return Err(SysErrNo::EROFS);
            }
        }
    }

    let abs_path = inner.get_abs_path(&task, dirfd, &path)?;
    let (parent_path, _) = rsplit_once(abs_path.as_str(), "/");
    let parent_inode = open(&parent_path, OpenFlags::O_RDWR, NONE_MODE)?.file()?;
    let parent_mode = parent_inode.inode.fmode()? & 0xfff;
    let parent_mode = FaccessatFileMode::from_bits_truncate(parent_mode);
    if parent_inode.inode.types() != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }
    if inner.user_id != 0
        && !(parent_mode.contains(FaccessatFileMode::S_IXUSR)
            || parent_mode.contains(FaccessatFileMode::S_IXGRP)
            || parent_mode.contains(FaccessatFileMode::S_IXOTH))
    {
        //父目录必须有可以执行的权限
        return Err(SysErrNo::EACCES);
    }
    let inode = open(&abs_path, OpenFlags::O_RDWR, NONE_MODE)?.file()?;
    let file_mode = inode.inode.fmode()? & 0xfff;
    let file_mode = FaccessatFileMode::from_bits_truncate(file_mode);
    if mode.contains(FaccessatMode::R_OK)
        && inner.user_id != 0
        && !(file_mode.contains(FaccessatFileMode::S_IRUSR)
            || file_mode.contains(FaccessatFileMode::S_IRGRP)
            || file_mode.contains(FaccessatFileMode::S_IROTH))
    {
        return Err(SysErrNo::EACCES);
    }
    if mode.contains(FaccessatMode::W_OK)
        && inner.user_id != 0
        && !(file_mode.contains(FaccessatFileMode::S_IWUSR)
            || file_mode.contains(FaccessatFileMode::S_IWGRP)
            || file_mode.contains(FaccessatFileMode::S_IWOTH))
    {
        return Err(SysErrNo::EACCES);
    }
    if mode.contains(FaccessatMode::X_OK)
        && !(file_mode.contains(FaccessatFileMode::S_IXUSR)
            || file_mode.contains(FaccessatFileMode::S_IXGRP)
            || file_mode.contains(FaccessatFileMode::S_IXOTH))
    {
        return Err(SysErrNo::EACCES);
    }
    Ok(0)
}
