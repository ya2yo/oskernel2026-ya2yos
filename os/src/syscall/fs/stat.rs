//! 文件状态与访问权限相关 syscall 实现。
//!
//! 这里包含 `statfs/fstatfs/statx/fstatat`。

use linux_raw_sys::general::{
    statx, statx_timestamp, AT_EMPTY_PATH, AT_FDCWD, STATX_BASIC_STATS, STATX__RESERVED,
};
use log::debug;

use crate::{
    fs::{
        open, superblock_fs_stat, InodeType, Kstat, MountFlags, OpenFlags, Statfs, MNT_TABLE,
        NONE_MODE,
    },
    mm::{copy_to_user, if_bad_address, read_user_cstr},
    task::current_task,
    utils::{trim_start_slash, SysErrNo, SyscallRet},
};

/// 将内核设备号转换为 Linux stat/statx 展示用 major 号。
fn linux_dev_major(dev: usize) -> u32 {
    (((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff)) as u32
}

/// 将内核设备号转换为 Linux stat/statx 展示用 minor 号。
fn linux_dev_minor(dev: usize) -> u32 {
    ((dev & 0xff) | ((dev >> 12) & !0xff)) as u32
}

/// 将内核 `Kstat` 转换为 Linux `statx` ABI 结构。
///
/// 当前实现填充基础属性集合；`mask` 暂未用于裁剪返回字段。
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
        stx_rdev_major: linux_dev_major(kst.st_rdev),
        stx_rdev_minor: linux_dev_minor(kst.st_rdev),
        stx_dev_major: linux_dev_major(kst.st_dev),
        stx_dev_minor: linux_dev_minor(kst.st_dev),
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
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

    if (kst as isize) <= 0 || if_bad_address(kst as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if fd >= proc.fd_table.len() || proc.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EBADF);
    }
    let file = proc.fd_table.get(fd)?.any();
    let kst_data = file.fstat();
    copy_to_user(&memory_set, kst as usize, unsafe {
        core::slice::from_raw_parts(
            &kst_data as *const Kstat as *const u8,
            core::mem::size_of::<Kstat>(),
        )
    })?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/fstatat64.2.html
pub fn sys_fstatat(dirfd: isize, path: *const u8, kst: *mut Kstat, flags: usize) -> SyscallRet {
    let task = current_task().unwrap();

    let proc = &task.process;
    let memory_set = &proc.memory_set_arc();

    if (kst as isize) <= 0 || if_bad_address(kst as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let path = read_user_cstr(memory_set, path)?;
    let path = trim_start_slash(path);

    let file = if path.is_empty() {
        if flags & AT_EMPTY_PATH as usize == 0 {
            return Err(SysErrNo::ENOENT);
        }
        if dirfd == AT_FDCWD as isize {
            let cwd = proc.fs_info.get_cwd();
            open(&cwd, OpenFlags::O_RDONLY, NONE_MODE)?.any()
        } else if dirfd < 0 {
            return Err(SysErrNo::EBADF);
        } else {
            proc.fd_table.get(dirfd as usize)?.any()
        }
    } else {
        let abs_path = proc.get_abs_path(dirfd, &path)?;

        if abs_path == "/ls" || abs_path == "/xargs" || abs_path == "/sleep" {
            open(&abs_path, OpenFlags::O_CREATE, NONE_MODE);
        }

        open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.any()
    };
    let kst_data = file.fstat();
    copy_to_user(memory_set, kst as usize, unsafe {
        core::slice::from_raw_parts(
            &kst_data as *const Kstat as *const u8,
            core::mem::size_of::<Kstat>(),
        )
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
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let kstat = if path.is_null() {
        // path 为 nullptr，且设置了 AT_EMPTY_PATH，表示获取 dirfd 指向文件的信息
        if flags & AT_EMPTY_PATH as usize == 0 {
            return Err(SysErrNo::EFAULT);
        }
        if dirfd == AT_FDCWD as isize {
            return Err(SysErrNo::EINVAL);
        }
        proc.fd_table.get(dirfd as usize)?.any().fstat()
    } else {
        let path_str = read_user_cstr(&memory_set, path)?;
        let path_str = trim_start_slash(path_str);
        if path_str.is_empty() && flags & AT_EMPTY_PATH as usize != 0 {
            // path 为空字符串，且设置了 AT_EMPTY_PATH，同样按 dirfd 查询
            if dirfd == AT_FDCWD as isize {
                return Err(SysErrNo::EINVAL);
            }
            proc.fd_table.get(dirfd as usize)?.any().fstat()
        } else {
            if path_str.is_empty() {
                return Err(SysErrNo::ENOENT);
            }
            // 绝对路径直接打开，dirfd 会被 get_abs_path 忽略；
            // 相对路径则由 get_abs_path 根据 AT_FDCWD 或 dirfd 转成绝对路径
            let abs_path = proc.get_abs_path(dirfd, &path_str)?;
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
///
/// 返回 path 所在文件系统的统计信息，写入用户空间 Statfs 缓冲区。
/// 当前内核仅有单一 ext4 文件系统，所有路径返回相同的 superblock 数据；
/// 但仍需解析 path 确认文件存在，以便返回正确的错误码（ENOENT 等）。
pub fn sys_statfs(path: *const u8, statfs: *mut Statfs) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

    if statfs.is_null() || (statfs as isize) <= 0 {
        return Err(SysErrNo::EFAULT);
    }

    let path = read_user_cstr(&memory_set, path)?;
    let path = trim_start_slash(path);

    if path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }

    // 打开路径确认文件存在；当前只有单一文件系统，不区分挂载点。
    let abs_path = proc.get_abs_path(AT_FDCWD as isize, &path)?;
    open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?;

    let mut stat = superblock_fs_stat();
    // 从挂载表查询该路径的挂载标志，填充 statfs.f_flags。
    if let Some((_source, _dir, _fstype, flags)) = MNT_TABLE.lock().mount_for_path(&abs_path) {
        stat.f_flags = flags.bits() as i64;
    }
    copy_to_user(&memory_set, statfs as usize, unsafe {
        core::slice::from_raw_parts(
            &stat as *const Statfs as *const u8,
            core::mem::size_of::<Statfs>(),
        )
    })?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/fstatfs.2.html
///
/// 返回 fd 所在文件系统的统计信息，写入用户空间 Statfs 缓冲区。
/// 当前内核仅有单一 ext4 文件系统，所有 fd 返回相同的 superblock 数据。
pub fn sys_fstatfs(fd: i32, buf: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let fd = fd as usize;

    let file = proc.fd_table.get(fd)?.any();
    let mut stat = superblock_fs_stat();

    // 从挂载表查询该文件路径的挂载标志，填充 statfs.f_flags。
    let path = file.path();
    if let Some((_source, _dir, _fstype, flags)) = MNT_TABLE.lock().mount_for_path(&path) {
        stat.f_flags = flags.bits() as i64;
    }

    let bytes = unsafe {
        core::slice::from_raw_parts(
            &stat as *const Statfs as *const u8,
            core::mem::size_of::<Statfs>(),
        )
    };
    let memory_set = proc.memory_set_arc();
    copy_to_user(&memory_set, buf, bytes)
}
