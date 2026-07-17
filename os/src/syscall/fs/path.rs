//! 路径读取、路径权限检查与进程文件系统上下文相关 syscall。

use alloc::string::String;
use alloc::vec;

use linux_raw_sys::general::{AT_EACCESS, AT_EMPTY_PATH, AT_FDCWD, AT_SYMLINK_NOFOLLOW};
use log::debug;

use crate::{
    fs::{open, InodeType, Kstat, OpenFlags, MAX_PATH_LEN, MNT_TABLE, NONE_MODE},
    mm::{copy_to_user, if_bad_address, read_user_cstr},
    syscall::options::{FaccessatFileMode, FaccessatMode},
    task::current_task,
    utils::{get_abs_path, is_abs_path, rsplit_once, SysErrNo, SyscallRet},
};

pub(super) fn parse_proc_self_fd(path: &str) -> Option<usize> {
    path.strip_prefix("/proc/self/fd/")
        .and_then(|fd| fd.parse::<usize>().ok())
}

/// 参考 https://man7.org/linux/man-pages/man2/getcwd.2.html
pub fn sys_getcwd(buf: *const u8, size: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let cwd = proc.fs_info.get_cwd();
    let cwd_bytes = cwd.as_bytes();
    let cwd_len_with_null = cwd_bytes.len() + 1;
    if size < cwd_len_with_null {
        return Err(SysErrNo::ERANGE);
    }
    let memory_set = proc.memory_set_arc();
    let mut cwd_with_null = vec![0u8; cwd_len_with_null];
    cwd_with_null[..cwd_bytes.len()].copy_from_slice(cwd_bytes);
    copy_to_user(&memory_set, buf as usize, &cwd_with_null)?;
    Ok(cwd_len_with_null)
}

/// 参考 https://man7.org/linux/man-pages/man2/chdir.2.html
pub fn sys_chdir(path: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

    if (path as isize) <= 0 || if_bad_address(path as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let path = read_user_cstr(&memory_set, path)?;

    // MAX_PATH_LEN includes the trailing NUL. read_user_cstr() returns a
    // MAX_PATH_LEN-byte string when no terminator is found within the limit.
    if path.len() >= MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    let locked_fs_info = &proc.fs_info;
    let abs_path = get_abs_path(&locked_fs_info.get_cwd(), &path);
    let osfile = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
    if !osfile.inode.types().is_dir() {
        return Err(SysErrNo::ENOTDIR);
    }

    let (euid, egid) = {
        let task_inner = task.inner_lock();
        (task_inner.effective_uid, task_inner.effective_gid)
    };
    check_directory_search_permission(&osfile.inode.path(), euid, egid)?;

    // `open()` follows the final symlink. Keep the resolved directory path so
    // getcwd() reports the directory itself rather than the symlink alias.
    locked_fs_info.set_cwd(osfile.inode.path());

    Ok(0)
}

/// `chdir(2)` requires search permission on every directory in the resolved
/// path, including the destination directory itself.
fn check_directory_search_permission(path: &str, uid: u32, gid: u32) -> SyscallRet {
    if uid == 0 {
        return Ok(0);
    }

    let mut current = String::from("/");
    for component in path.split('/').filter(|component| !component.is_empty()) {
        if current.len() > 1 {
            current.push('/');
        }
        current.push_str(component);

        let directory = open(&current, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
        if !directory.inode.types().is_dir() {
            return Err(SysErrNo::ENOTDIR);
        }
        let stat = directory.inode.fstat();
        let mode = FaccessatFileMode::from_bits_truncate(directory.inode.fmode()? & 0xfff);
        if !mode_allows(
            mode,
            &stat,
            uid,
            gid,
            FaccessatFileMode::S_IXUSR,
            FaccessatFileMode::S_IXGRP,
            FaccessatFileMode::S_IXOTH,
        ) {
            return Err(SysErrNo::EACCES);
        }
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/chroot.2.html
pub fn sys_chroot(path: *const u8) -> SyscallRet {
    debug!("[chroot] path=0x{:x}", path as usize);

    let task = current_task().unwrap();

    if path.is_null() {
        return Err(SysErrNo::EINVAL);
    }
    if (path as isize) <= 0 || if_bad_address(path as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if task.inner_lock().user_id != 0 {
        return Err(SysErrNo::EPERM);
    }

    let path_str = {
        let proc = &task.process;
        let memory_set = proc.memory_set_arc();
        read_user_cstr(&memory_set, path)?
    };

    let file = open(&path_str, OpenFlags::O_RDONLY, NONE_MODE)?;
    let osfile = file.file()?;
    if osfile.inode.types() != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }

    task.process.fs_info.set_cwd(path_str);
    debug!("[chroot] success");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/readlinkat.2.html
pub fn sys_readlinkat(dirfd: isize, path: *const u8, buf: *const u8, bufsize: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let path = read_user_cstr(&memory_set, path)?;

    if path == "/proc/self/exe" {
        let mut exe: String = proc.fs_info.get_exe();
        exe.push('\0');

        let res = exe.len();
        let mem = proc.memory_set_arc();
        copy_to_user(&*mem, buf as usize, exe.as_bytes())?;
        return Ok(res);
    }

    let bufsize = core::cmp::min(bufsize, 4096usize);
    let abs_path = proc.get_abs_path(dirfd, &path)?;
    if let Some(fd) = parse_proc_self_fd(&abs_path) {
        proc.fd_table.get(fd)?;
        let target = proc.fs_info.fd_path(fd).ok_or(SysErrNo::ENOENT)?;
        let readcnt = target.len().min(bufsize);
        copy_to_user(&*memory_set, buf as usize, &target.as_bytes()[..readcnt])?;
        return Ok(readcnt);
    }
    let mut linkbuf = vec![0u8; bufsize];
    // readlinkat() operates on the link itself instead of its target.
    let file = open(&abs_path, OpenFlags::O_UNLINK, NONE_MODE)?.file()?;
    if !file.inode.types().is_symlink() {
        return Err(SysErrNo::EINVAL);
    }
    let readcnt = file.inode.read_link(&mut linkbuf, bufsize)?;
    let mem = proc.memory_set_arc();
    copy_to_user(&*mem, buf as usize, &linkbuf[..readcnt])?;
    Ok(readcnt)
}

/// 按 POSIX owner/group/other 顺序检查一个权限位是否允许当前凭据访问。
pub(super) fn mode_allows(
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

fn has_too_long_path_component(path: &str) -> bool {
    path.split('/')
        .any(|component| component.len() > MAX_FILE_NAME_LEN)
}

/// `faccessat2` 当前支持的 Linux flags。
const FACCESSAT2_VALID_FLAGS: usize = (AT_EACCESS | AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) as usize;

/// `faccessat` 和 `faccessat2` 的共用实现。
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
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

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
        proc.fd_table.get(dirfd as usize)?;
    }

    if path.is_empty() {
        let file = if dirfd == AT_FDCWD {
            let cwd = proc.fs_info.get_cwd();
            open(&cwd, OpenFlags::O_RDONLY, NONE_MODE)?.any()
        } else if dirfd < 0 {
            return Err(SysErrNo::EBADF);
        } else {
            proc.fd_table.get(dirfd as usize)?.any()
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

    let abs_path = proc.get_abs_path(dirfd as isize, &path)?;
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
                // 只读文件系统挂载
                return Err(SysErrNo::EROFS);
            }
        }
    }
    let file_mode = inode.inode.fmode()? & 0xfff;
    let file_mode = FaccessatFileMode::from_bits_truncate(file_mode);
    let file_stat = inode.inode.fstat();
    check_faccessat_access(file_mode, &file_stat, uid, gid, mode)
}

/// 参考 https://man7.org/linux/man-pages/man2/faccessat.2.html
pub fn sys_faccessat(dirfd: i32, path: *const u8, mode: u32, _flags: usize) -> SyscallRet {
    do_faccessat(dirfd, path, mode, 0)
}

/// 参考 https://man7.org/linux/man-pages/man2/faccessat2.2.html
pub fn sys_faccessat2(dirfd: i32, path: *const u8, mode: u32, flags: usize) -> SyscallRet {
    if flags & !FACCESSAT2_VALID_FLAGS != 0 {
        return Err(SysErrNo::EINVAL);
    }
    do_faccessat(dirfd, path, mode, flags)
}
