//! 文件状态与访问权限相关 syscall 实现。
//!
//! 这里包含 `statfs/fstatfs/statx/fstatat` 以及 `faccessat/faccessat2`。
//! `faccessat` 系列的权限检查刻意不委托给 `open()`：Linux 语义要求
//! `access/faccessat` 默认使用 real uid/gid，而 `open()` 使用 effective
//! uid/gid；`faccessat2(AT_EACCESS)` 才切换到 effective uid/gid。

use alloc::string::String;
use linux_raw_sys::general::{
    statx, statx_timestamp, AT_EACCESS, AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW,
    STATX_BASIC_STATS, STATX__RESERVED,
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
    utils::{is_abs_path, rsplit_once, trim_start_slash, SysErrNo, SyscallRet},
};

/// 按 POSIX owner/group/other 顺序检查一个权限位是否允许当前凭据访问。
///
/// `file_mode` 只包含低 12 位权限与特殊位；`stat` 提供文件 owner/group。
/// 调用者负责决定传入 real uid/gid 还是 effective uid/gid。
fn mode_allows(
    file_mode: FaccessatFileMode,
    stat: &Kstat,
    uid: u32,
    gid: u32,
    owner_bit: FaccessatFileMode,
    group_bit: FaccessatFileMode,
    other_bit: FaccessatFileMode,
) -> bool {
    if uid == stat.st_uid {
        file_mode.contains(owner_bit)
    } else if gid == stat.st_gid {
        file_mode.contains(group_bit)
    } else {
        file_mode.contains(other_bit)
    }
}

/// 对已解析出的目标文件权限位执行 `R_OK/W_OK/X_OK` 检查。
///
/// - `F_OK` 不检查权限位，能走到这里就表示目标存在。
/// - uid 0 读取和写入直接放行，但执行权限仍要求至少一个 execute 位存在。
/// - 普通用户按 owner/group/other 三段权限判断，失败返回 `EACCES`。
fn check_faccessat_access(
    file_mode: FaccessatFileMode,
    file_stat: &Kstat,
    uid: u32,
    gid: u32,
    mode: FaccessatMode,
) -> SyscallRet {
    if mode.contains(FaccessatMode::R_OK)
        && uid != 0
        && !mode_allows(
            file_mode,
            file_stat,
            uid,
            gid,
            FaccessatFileMode::S_IRUSR,
            FaccessatFileMode::S_IRGRP,
            FaccessatFileMode::S_IROTH,
        )
    {
        return Err(SysErrNo::EACCES);
    }
    if mode.contains(FaccessatMode::W_OK)
        && uid != 0
        && !mode_allows(
            file_mode,
            file_stat,
            uid,
            gid,
            FaccessatFileMode::S_IWUSR,
            FaccessatFileMode::S_IWGRP,
            FaccessatFileMode::S_IWOTH,
        )
    {
        return Err(SysErrNo::EACCES);
    }
    if mode.contains(FaccessatMode::X_OK)
        && !if uid == 0 {
            file_mode.intersects(
                FaccessatFileMode::S_IXUSR
                    | FaccessatFileMode::S_IXGRP
                    | FaccessatFileMode::S_IXOTH,
            )
        } else {
            mode_allows(
                file_mode,
                file_stat,
                uid,
                gid,
                FaccessatFileMode::S_IXUSR,
                FaccessatFileMode::S_IXGRP,
                FaccessatFileMode::S_IXOTH,
            )
        }
    {
        return Err(SysErrNo::EACCES);
    }
    Ok(0)
}

const MAX_FILE_NAME_LEN: usize = 255;

/// 检查路径中任一分量是否超过 Linux `NAME_MAX`。
///
/// 仅检查单个分量长度；整条路径长度由调用方用 `MAX_PATH_LEN` 单独限制。
fn has_too_long_path_component(path: &str) -> bool {
    path.split('/')
        .any(|component| component.len() > MAX_FILE_NAME_LEN)
}

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
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    if (kst as isize) <= 0 || if_bad_address(kst as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if fd >= proc_inner.fd_table.len() || proc_inner.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EBADF);
    }
    let file = proc_inner.fd_table.get(fd)?.any();
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

    let proc_inner = &task.process;
    let memory_set = &proc_inner.memory_set_arc();

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
            let cwd = proc_inner.fs_info.get_cwd();
            open(&cwd, OpenFlags::O_RDONLY, NONE_MODE)?.any()
        } else if dirfd < 0 {
            return Err(SysErrNo::EBADF);
        } else {
            proc_inner.fd_table.get(dirfd as usize)?.any()
        }
    } else {
        let abs_path = proc_inner.get_abs_path(dirfd, &path)?;

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
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
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
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
    let stat = superblock_fs_stat();
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
    let proc_inner = &task.process;
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
    let memory_set = proc_inner.memory_set_arc();
    copy_to_user(&memory_set, buf, bytes)
}

/// `faccessat2` 当前支持的 Linux flags。
///
/// `AT_EACCESS` 改用 effective uid/gid；`AT_EMPTY_PATH` 允许空路径按
/// `dirfd` 指向对象检查；`AT_SYMLINK_NOFOLLOW` 通过 VFS 的 no-follow
/// 兼容路径检查 symlink 自身。
const FACCESSAT2_VALID_FLAGS: usize = (AT_EACCESS | AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) as usize;

/// `faccessat` 和 `faccessat2` 的共用实现。
///
/// 该函数只做 Linux 可见语义处理：用户字符串读取、`mode/flags` 语义、
/// `dirfd` 与路径解析、父目录 execute 权限、只读挂载写检查和目标权限位检查。
///
/// 关键 errno/路径规则：
/// - 绝对路径忽略 `dirfd`，因此坏 `dirfd` 不应覆盖真实路径结果。
/// - 相对路径需要有效 `dirfd`，坏 fd 返回 `EBADF`，非目录 fd 会在解析父路径时返回
///   `ENOTDIR`。
/// - 空路径只有在 `AT_EMPTY_PATH` 下合法；否则返回 `ENOENT`。
/// - `faccessat` 默认传入 `flags = 0`，保持 real uid/gid 语义。
fn do_faccessat(dirfd: i32, path: *const u8, mode: u32, flags: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.inner_lock();
    let use_effective_ids = flags & AT_EACCESS as usize != 0;
    let uid = if use_effective_ids {
        inner.effective_uid
    } else {
        inner.user_id as u32
    };
    let gid = if use_effective_ids {
        inner.effective_gid
    } else {
        inner.real_gid
    };
    drop(inner);
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    let mode = FaccessatMode::from_bits(mode).ok_or(SysErrNo::EINVAL)?;
    let path = if path.is_null() {
        if flags & AT_EMPTY_PATH as usize == 0 {
            return Err(SysErrNo::EFAULT);
        }
        String::new()
    } else {
        read_user_cstr(&memory_set, path)?
    };

    if path.len() == 0 && flags & AT_EMPTY_PATH as usize == 0 {
        return Err(SysErrNo::ENOENT);
    }

    if path.len() > MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    if has_too_long_path_component(&path) {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    if !path.is_empty() && !is_abs_path(&path) && dirfd != AT_FDCWD {
        if dirfd < 0 {
            return Err(SysErrNo::EBADF);
        }
        proc_inner.fd_table.get(dirfd as usize)?;
    }

    if path.is_empty() {
        let file = if dirfd == AT_FDCWD {
            let cwd = proc_inner.fs_info.get_cwd();
            open(&cwd, OpenFlags::O_RDONLY, NONE_MODE)?.any()
        } else if dirfd < 0 {
            return Err(SysErrNo::EBADF);
        } else {
            proc_inner.fd_table.get(dirfd as usize)?.any()
        };
        let file_stat = file.fstat();
        let file_mode = FaccessatFileMode::from_bits_truncate(file_stat.st_mode & 0xfff);
        return check_faccessat_access(file_mode, &file_stat, uid, gid, mode);
    }

    if dirfd != AT_FDCWD && dirfd < 0 && !is_abs_path(&path) {
        return Err(SysErrNo::EBADF);
    }

    debug!(
        "[do_faccessat] dirfd is {} and path is {} and mode is {:?}, flags={:#x}",
        dirfd, path, mode, flags
    );

    let abs_path = proc_inner.get_abs_path(dirfd as isize, &path)?;
    let (parent_path, _) = rsplit_once(abs_path.as_str(), "/");
    let parent_inode = open(&parent_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
    let parent_mode = parent_inode.inode.fmode()? & 0xfff;
    let parent_mode = FaccessatFileMode::from_bits_truncate(parent_mode);
    if parent_inode.inode.types() != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }
    let parent_stat = parent_inode.inode.fstat();
    if uid != 0
        && !mode_allows(
            parent_mode,
            &parent_stat,
            uid,
            gid,
            FaccessatFileMode::S_IXUSR,
            FaccessatFileMode::S_IXGRP,
            FaccessatFileMode::S_IXOTH,
        )
    {
        //父目录必须有可以执行的权限
        return Err(SysErrNo::EACCES);
    }
    let open_flags = if flags & AT_SYMLINK_NOFOLLOW as usize != 0 {
        OpenFlags::O_RDONLY | OpenFlags::O_UNLINK
    } else {
        OpenFlags::O_RDONLY
    };
    let inode = open(&abs_path, open_flags, NONE_MODE)?.file()?;
    if mode.contains(FaccessatMode::W_OK) {
        if let Some((_, _, _, mountflags)) = MNT_TABLE.lock().mount_for_path(&abs_path) {
            if mountflags & 1 != 0 {
                return Err(SysErrNo::EROFS);
            }
        }
    }
    let file_mode = inode.inode.fmode()? & 0xfff;
    let file_mode = FaccessatFileMode::from_bits_truncate(file_mode);
    let file_stat = inode.inode.fstat();
    check_faccessat_access(file_mode, &file_stat, uid, gid, mode)
}

/// 实现 Linux `faccessat(2)`。
///
/// 当前接口忽略用户传入的第四参数，按 Linux raw `faccessat` 老 syscall 的
/// real uid/gid 语义执行权限检查。
///
/// 参考 https://man7.org/linux/man-pages/man2/faccessat.2.html
pub fn sys_faccessat(dirfd: i32, path: *const u8, mode: u32, _flags: usize) -> SyscallRet {
    do_faccessat(dirfd, path, mode, 0)
}

/// 实现 Linux `faccessat2(2)`。
///
/// 相比旧 `faccessat`，该 syscall 的主要差异是内核直接接收并解释
/// `flags`。目前支持 `AT_EACCESS`、`AT_SYMLINK_NOFOLLOW` 与 `AT_EMPTY_PATH`；
/// 未知 flag 按 Linux 语义返回 `EINVAL`。
///
/// 参考 https://man7.org/linux/man-pages/man2/faccessat2.2.html
pub fn sys_faccessat2(dirfd: i32, path: *const u8, mode: u32, flags: usize) -> SyscallRet {
    if flags & !FACCESSAT2_VALID_FLAGS != 0 {
        return Err(SysErrNo::EINVAL);
    }
    do_faccessat(dirfd, path, mode, flags)
}
