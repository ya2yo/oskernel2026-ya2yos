use super::*;

/// 修改 inode 的 uid/gid 元数据，是 `chown` 系列 syscall 的公共实现。
///
/// 只有 effective uid 为 0 的任务可修改 owner/group；`usize::MAX` 表示 Linux
/// ABI 中的 `(uid_t)-1` 或 `(gid_t)-1`，即保持对应字段不变。
/// 参考 https://www.man7.org/linux/man-pages/man2/fchownat.2.html
fn chown_inode(
    inode: Arc<dyn Inode>,
    path: Option<&str>,
    owner: usize,
    group: usize,
) -> SyscallRet {
    if let Some(path) = path {
        if let Some((_, _, _, mountflags)) = MNT_TABLE.lock().mount_for_path(path) {
            if mountflags & 1 != 0 {
                return Err(SysErrNo::EROFS);
            }
        }
    }

    let task = current_task().unwrap();
    {
        let task_inner = task.inner_lock();
        if task_inner.effective_uid != 0 {
            return Err(SysErrNo::EPERM);
        }
    }

    let stat = inode.fstat();
    // POSIX uses (uid_t)-1/(gid_t)-1 to mean "leave this field unchanged".
    let uid = if owner == usize::MAX {
        stat.st_uid
    } else {
        if owner > u32::MAX as usize {
            return Err(SysErrNo::EINVAL);
        }
        owner as u32
    };
    let gid = if group == usize::MAX {
        stat.st_gid
    } else {
        if group > u32::MAX as usize {
            return Err(SysErrNo::EINVAL);
        }
        group as u32
    };

    inode.owner_set(uid, gid)?;

    // Linux clears S_ISUID on a successful chown of a non-directory inode.
    // S_ISGID is only cleared when the group-execute bit is set; otherwise it
    // may represent the mandatory-locking marker and must be preserved.
    if inode.types() != InodeType::Dir {
        let mode = stat.st_mode;
        let mut new_mode = mode & !FaccessatFileMode::S_ISUID.bits();
        if mode & FaccessatFileMode::S_IXGRP.bits() != 0 {
            new_mode &= !FaccessatFileMode::S_ISGID.bits();
        }
        if new_mode != mode {
            inode.fmode_set(new_mode)?;
        }
    }

    Ok(0)
}

/// 修改 inode 的权限 mode，是 `chmod` 系列 syscall 的公共实现。
///
/// 该 helper 处理 chmod 的共享语义：已知路径位于只读挂载点时返回 `EROFS`，非 root
/// 且非 inode owner 时返回 `EPERM`；非 root 且 effective gid 不匹配 inode gid 时
/// 按 Linux 语义静默清除请求中的 `S_ISGID`。最终错误由底层 `fmode_set()` 传播。
fn chmod_inode(inode: Arc<dyn Inode>, path: Option<&str>, mode: u32) -> SyscallRet {
    if let Some(path) = path {
        if let Some((_, _, _, mountflags)) = MNT_TABLE.lock().mount_for_path(path) {
            if mountflags & 1 != 0 {
                return Err(SysErrNo::EROFS);
            }
        }
    }

    let stat = inode.fstat();
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let euid = task_inner.effective_uid;
    let egid = task_inner.effective_gid;
    drop(task_inner);

    if euid != 0 && euid != stat.st_uid {
        return Err(SysErrNo::EPERM);
    }

    let mut new_mode = mode;
    if euid != 0 && egid != stat.st_gid {
        new_mode &= !0o2000;
    }

    inode.fmode_set(new_mode)
}

/// 实现 `fchownat(2)`，按路径、fd 或 `AT_EMPTY_PATH` 修改文件 uid/gid。
///
/// 空路径需要 `AT_EMPTY_PATH` 并以 dirfd 指向的已打开文件为目标；`/proc/self/fd/<fd>`
/// 路径按 fd 语义处理，以保持 O_PATH fd 返回 `EBADF` 的行为；普通路径可按 flags
/// 选择是否跟随符号链接。
/// 参考 https://www.man7.org/linux/man-pages/man2/fchownat.2.html
pub fn sys_fchownat(
    dirfd: isize,
    pathname: *const u8,
    owner: usize,
    group: usize,
    flags: u32,
) -> SyscallRet {
    // chown/fchown wrappers reach this syscall; do not report success without
    // updating the inode owner, or stat() observes stale uid/gid.
    let valid_flags = (AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) as u32;
    if flags & !valid_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let path = read_user_cstr(&memory_set, pathname)?;
    if path.len() > MAX_PATH_LEN || has_too_long_path_component(&path) {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    let (inode, resolved_path) = if path.is_empty() {
        if flags & AT_EMPTY_PATH as u32 == 0 {
            return Err(SysErrNo::ENOENT);
        }
        if dirfd < 0 {
            return Err(SysErrNo::EBADF);
        }
        let fd_desc = proc.fd_table.get(dirfd as usize)?;
        // fchown(fd, ...) operates on file contents; an O_PATH fd is only a path handle.
        if fd_desc.is_path_only() {
            return Err(SysErrNo::EBADF);
        }
        let file = fd_desc.file()?;
        let path = file.inode.path();
        (file.inode.clone(), Some(path))
    } else {
        let abs_path = proc.get_abs_path(dirfd, &path)?;
        if let Some(fd) = parse_proc_self_fd(&abs_path) {
            let fd_desc = proc.fd_table.get(fd)?;
            // musl may implement fchown(fd, ...) through /proc/self/fd/<fd>.
            // Preserve fd semantics so O_PATH still fails with EBADF.
            if fd_desc.is_path_only() {
                return Err(SysErrNo::EBADF);
            }
            let file = fd_desc.file()?;
            let path = file.inode.path();
            (file.inode.clone(), Some(path))
        } else {
            let (parent_path, _) = rsplit_once(abs_path.as_str(), "/");
            check_parent_permission(parent_path, false)?;
            let open_flags = if flags & AT_SYMLINK_NOFOLLOW as u32 != 0 {
                OpenFlags::O_NOFOLLOW
            } else {
                OpenFlags::empty()
            };
            let file = open(&abs_path, open_flags, NONE_MODE)?.file()?;
            (file.inode.clone(), Some(abs_path))
        }
    };

    chown_inode(inode, resolved_path.as_deref(), owner, group)
}
/// 实现 `fchown(2)`，通过已打开文件描述符修改文件 uid/gid。
///
/// `O_PATH` fd 只作为路径句柄，不代表可修改的已打开文件，因此返回 `EBADF`；其余
/// 语义委托给 `chown_inode()`。
/// 参考 https://www.man7.org/linux/man-pages/man2/fchown.2.html
pub fn sys_fchown(fd: usize, owner: usize, group: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let fd_desc = proc.fd_table.get(fd)?;
    // fchown(2) 修改已打开文件；O_PATH fd 只是路径句柄，Linux 返回 EBADF。
    if fd_desc.is_path_only() {
        return Err(SysErrNo::EBADF);
    }
    let file = fd_desc.file()?;
    let path = file.inode.path();
    chown_inode(file.inode.clone(), Some(&path), owner, group)
}
/// 实现 `fchmod(2)`，通过已打开文件描述符修改文件权限位。
///
/// `O_PATH` fd 返回 `EBADF`；普通 fd 解析出 inode 和路径后交给 `chmod_inode()` 处理
/// owner/root 权限检查、只读挂载检查和 `S_ISGID` 清除规则。
/// 参考 https://www.man7.org/linux/man-pages/man2/fchmod.2.html
pub fn sys_fchmod(fd: usize, mode: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;

    if (fd as isize) < 0 && fd >= proc.fd_table.len() {
        return Err(SysErrNo::EBADF);
    }

    // debug!("[sys_fchmod] fd is {},new mode is {:o}", fd, mode);

    let fd_desc = proc.fd_table.get(fd)?;
    // O_PATH fd 不代表已打开文件，fchmod(2) 需要返回 EBADF。
    if fd_desc.is_path_only() {
        return Err(SysErrNo::EBADF);
    }
    let file = fd_desc.file()?;
    let path = file.inode.path();
    chmod_inode(file.inode.clone(), Some(&path), mode)
}
/// `fchmodat` / `fchmodat2` 的公共实现，按 dirfd/path/flags 修改文件权限位。
///
/// 支持 `AT_EMPTY_PATH` 的 fd 目标、`/proc/self/fd/<fd>` 兼容路径和普通路径目标；
/// 各路径最终统一调用 `chmod_inode()`，保证和 `fchmod(2)` 一致的权限及 setgid 语义。
fn do_fchmodat(dirfd: isize, path: *const u8, mode: u32, flags: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

    if (flags as isize) < 0 {
        return Err(SysErrNo::EINVAL);
    }

    if dirfd != -100 && dirfd as usize >= proc.fd_table.len() {
        return Err(SysErrNo::EBADF);
    }

    if (path as isize) <= 0 || if_bad_address(path as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let path = read_user_cstr(&memory_set, path)?;

    if path.len() > MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    if path.len() == 0 && flags & AT_EMPTY_PATH as u32 == 0 {
        return Err(SysErrNo::ENOENT);
    }

    if path.len() == 0 {
        if dirfd < 0 {
            return Err(SysErrNo::EBADF);
        }
        let fd_desc = proc.fd_table.get(dirfd as usize)?;
        // fchmodat2(fd, "", ..., AT_EMPTY_PATH) 与 fchmod(fd, ...) 一样拒绝 O_PATH。
        if fd_desc.is_path_only() {
            return Err(SysErrNo::EBADF);
        }
        let file = fd_desc.file()?;
        let path = file.inode.path();
        return chmod_inode(file.inode.clone(), Some(&path), mode);
    }

    let abs_path = proc.get_abs_path(dirfd, &path)?;
    if let Some(fd) = parse_proc_self_fd(&abs_path) {
        let fd_desc = proc.fd_table.get(fd)?;
        // musl may implement fchmod(fd, ...) through /proc/self/fd/<fd>.
        // Preserve fd semantics so O_PATH still fails with EBADF.
        if fd_desc.is_path_only() {
            return Err(SysErrNo::EBADF);
        }
        let file = fd_desc.file()?;
        let path = file.inode.path();
        return chmod_inode(file.inode.clone(), Some(&path), mode);
    }

    debug!(
        "[do_fchmodat] path is {}, flags is {}, new mode is {:o}",
        &abs_path, flags, mode
    );

    let (parent_path, _) = rsplit_once(abs_path.as_str(), "/");
    let parent_inode = open(&parent_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
    if parent_inode.inode.types() != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }

    debug!(
        "{} set {:?}",
        abs_path,
        FaccessatFileMode::from_bits_truncate(mode)
    );

    let inode = open(&abs_path, OpenFlags::empty(), NONE_MODE)?.file()?;
    chmod_inode(inode.inode.clone(), Some(&abs_path), mode)
}

/// 实现 `fchmodat(2)`，按 dirfd/path/flags 修改文件权限位。
///
/// 仅支持 `AT_SYMLINK_NOFOLLOW`，不支持 `AT_EMPTY_PATH`（该标志由 `fchmodat2` 提供）。
/// 参考 https://www.man7.org/linux/man-pages/man2/fchmodat.2.html
pub fn sys_fchmodat(dirfd: isize, path: *const u8, mode: u32, flags: u32) -> SyscallRet {
    if flags & AT_EMPTY_PATH as u32 != 0 {
        return Err(SysErrNo::EINVAL);
    }
    do_fchmodat(dirfd, path, mode, flags)
}

/// 实现 `fchmodat2(2)`（Linux 6.6+），是 `fchmodat(2)` 的扩展版本。
///
/// 与 `fchmodat(2)` 相比，额外支持 `AT_EMPTY_PATH` 标志，允许对 fd 本身操作。
/// 参考 https://www.man7.org/linux/man-pages/man2/fchmodat2.2.html
pub fn sys_fchmodat2(dirfd: isize, path: *const u8, mode: u32, flags: u32) -> SyscallRet {
    do_fchmodat(dirfd, path, mode, flags)
}
