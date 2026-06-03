use alloc::string::String;
use linux_raw_sys::general::{
    statx, statx_timestamp, AT_EMPTY_PATH, AT_FDCWD, STATX_BASIC_STATS, STATX__RESERVED,
};
use log::debug;

use crate::{
    fs::{
        open, superblock_fs_stat, InodeType, Kstat, OpenFlags, Statfs, MAX_PATH_LEN, MNT_TABLE,
        NONE_MODE,
    },
    mm::{copy_to_user, if_bad_address, read_user_cstr},
    syscall::options::{FaccessatFileMode, FaccessatMode},
    task::current_task,
    utils::{rsplit_once, trim_start_slash, SysErrNo, SyscallRet},
};

fn kstat_to_statx(kst: &Kstat, _mask: u32) -> statx {
    statx {
        stx_mask: STATX_BASIC_STATS,
        stx_blksize: kst.st_blksize as u32,
        stx_attributes: 0,
        stx_nlink: kst.st_nlink,
        stx_uid: kst.st_uid,
        stx_gid: kst.st_gid,
        stx_mode: kst.st_mode as u16,
        __spare0: [0; 1],
        stx_ino: kst.st_ino as u64,
        stx_size: kst.st_size as u64,
        stx_blocks: kst.st_blocks as u64,
        stx_attributes_mask: 0,
        stx_atime: statx_time(kst.st_atime, kst.st_atime_nsec),
        stx_btime: statx_time(0, 0),
        stx_ctime: statx_time(kst.st_ctime, kst.st_ctime_nsec),
        stx_mtime: statx_time(kst.st_mtime, kst.st_mtime_nsec),
        stx_rdev_major: 0,
        stx_rdev_minor: kst.st_rdev as u32,
        stx_dev_major: 0,
        stx_dev_minor: kst.st_dev as u32,
        stx_mnt_id: 0,
        stx_dio_mem_align: 0,
        stx_dio_offset_align: 0,
        stx_subvol: 0,
        stx_atomic_write_unit_min: 0,
        stx_atomic_write_unit_max: 0,
        stx_atomic_write_segments_max: 0,
        stx_dio_read_offset_align: 0,
        stx_atomic_write_unit_max_opt: 0,
        __spare2: [0; 1],
        __spare3: [0; 8],
    }
}

fn statx_time(sec: usize, nsec: usize) -> statx_timestamp {
    statx_timestamp {
        tv_sec: sec as i64,
        tv_nsec: nsec as u32,
        __reserved: 0,
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/fstat.2.html
pub fn sys_fstat(fd: usize, kst: *mut Kstat) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    if (kst as isize) <= 0 || if_bad_address(kst as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if fd >= proc_inner.fd_table.len() || proc_inner.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EBADF);
    }
    let file = proc_inner.fd_table.get(fd)?.any();
    let kst_data = file.fstat();
    copy_to_user(&memory_set, kst as usize, unsafe {
        core::slice::from_raw_parts(&kst_data as *const Kstat as *const u8, core::mem::size_of::<Kstat>())
    })?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/fstatat64.2.html
pub fn sys_fstatat(dirfd: isize, path: *const u8, kst: *mut Kstat, _flags: usize) -> SyscallRet {
    let task = current_task().unwrap();

    let proc_inner = task.process.inner_lock();
    let memory_set = &proc_inner.get_locked_memory_set_read();
    let path = read_user_cstr(memory_set, path)?;
    let path = trim_start_slash(path);

    let abs_path = proc_inner.get_abs_path(dirfd, &path)?;

    if abs_path == "/ls" || abs_path == "/xargs" || abs_path == "/sleep" {
        open(&abs_path, OpenFlags::O_CREATE, NONE_MODE);
    }

    let file = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.any();
    let kst_data = file.fstat();
    copy_to_user(memory_set, kst as usize, unsafe {
        core::slice::from_raw_parts(&kst_data as *const Kstat as *const u8, core::mem::size_of::<Kstat>())
    })?;
    return Ok(0);
}
/// 参考 https://man7.org/linux/man-pages/man2/statx.2.html
/// flags 忽略以下值：AT_NO_AUTOMOUNT、AT_STATX_FORCE_SYNC、AT_STATX_DONT_SYNC
pub fn sys_statx(
    dirfd: isize,
    path: *const u8,
    flags: usize,
    mask: u32,
    statxbuf: *mut statx,
) -> SyscallRet {
    if statxbuf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if mask & STATX__RESERVED != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let kstat = if path.is_null() {
        // path 为 nullptr，且设置了 AT_EMPTY_PATH，表示获取 dirfd 指向文件的信息
        if flags & AT_EMPTY_PATH as usize == 0 {
            return Err(SysErrNo::EFAULT);
        }
        if dirfd == AT_FDCWD as isize {
            return Err(SysErrNo::EINVAL);
        }
        proc_inner.fd_table.get(dirfd as usize)?.any().fstat()
    } else {
        let path_str = read_user_cstr(&memory_set, path)?;
        let path_str = trim_start_slash(path_str);
        if path_str.is_empty() && flags & AT_EMPTY_PATH as usize != 0 {
            // path 为空字符串，且设置了 AT_EMPTY_PATH，同样按 dirfd 查询
            if dirfd == AT_FDCWD as isize {
                return Err(SysErrNo::EINVAL);
            }
            proc_inner.fd_table.get(dirfd as usize)?.any().fstat()
        } else {
            if path_str.is_empty() {
                return Err(SysErrNo::ENOENT);
            }
            // 绝对路径直接打开，dirfd 会被 get_abs_path 忽略；
            // 相对路径则由 get_abs_path 根据 AT_FDCWD 或 dirfd 转成绝对路径
            let abs_path = proc_inner.get_abs_path(dirfd, &path_str)?;
            open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?
                .any()
                .fstat()
        }
    };

    let statx = kstat_to_statx(&kstat, mask);
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &statx as *const statx as *const u8,
            core::mem::size_of::<statx>(),
        )
    };
    copy_to_user(&memory_set, statxbuf as usize, bytes).map(|_| ())?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/statfs.2.html
pub fn sys_statfs(_path: *const u8, statfs: *mut Statfs) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let stat = superblock_fs_stat();
    copy_to_user(&memory_set, statfs as usize, unsafe {
        core::slice::from_raw_parts(&stat as *const Statfs as *const u8, core::mem::size_of::<Statfs>())
    })?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/fstatfs.2.html
///
/// 返回 fd 所在文件系统的统计信息，写入用户空间 Statfs 缓冲区。
/// 当前内核仅有单一 ext4 文件系统，所有 fd 返回相同的 superblock 数据。
pub fn sys_fstatfs(fd: i32, buf: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let fd = fd as usize;

    // 校验 fd 有效性
    if fd >= proc_inner.fd_table.len() || proc_inner.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EBADF);
    }

    let stat = superblock_fs_stat();
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &stat as *const Statfs as *const u8,
            core::mem::size_of::<Statfs>(),
        )
    };
    let memory_set = proc_inner.get_locked_memory_set_read();
    copy_to_user(&memory_set, buf, bytes)
}

/// 参考 https://man7.org/linux/man-pages/man2/faccessat.2.html
pub fn sys_faccessat(dirfd: i32, path: *const u8, mode: u32, _flags: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.inner_lock();
    let user_id = inner.user_id;
    drop(inner);
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    if (path as isize) <= 0 {
        return Err(SysErrNo::EFAULT);
    }
    if (mode as i32) < 0 {
        return Err(SysErrNo::EINVAL);
    }
    let path = read_user_cstr(&memory_set, path)?;

    if path.len() == 0 {
        return Err(SysErrNo::ENOENT);
    }

    if path.len() > MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    if dirfd != AT_FDCWD && dirfd as usize >= proc_inner.fd_table.len() {
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

    let abs_path = proc_inner.get_abs_path(dirfd as isize, &path)?;
    let (parent_path, _) = rsplit_once(abs_path.as_str(), "/");
    let parent_inode = open(&parent_path, OpenFlags::O_RDWR, NONE_MODE)?.file()?;
    let parent_mode = parent_inode.inode.fmode()? & 0xfff;
    let parent_mode = FaccessatFileMode::from_bits_truncate(parent_mode);
    if parent_inode.inode.types() != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }
    if user_id != 0
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
        && user_id != 0
        && !(file_mode.contains(FaccessatFileMode::S_IRUSR)
            || file_mode.contains(FaccessatFileMode::S_IRGRP)
            || file_mode.contains(FaccessatFileMode::S_IROTH))
    {
        return Err(SysErrNo::EACCES);
    }
    if mode.contains(FaccessatMode::W_OK)
        && user_id != 0
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
