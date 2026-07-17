use super::*;

/// 实现 `linkat(2)`，为已有文件创建新的硬链接。
///
/// 支持普通路径硬链接和 `AT_EMPTY_PATH`/`/proc/self/fd/<fd>` 兼容路径；后者用于把
/// `O_TMPFILE` 风格的匿名文件 materialize 到目标路径。成功后更新 inode cache 和
/// dentry cache，保证新路径能复用原 inode。
/// 参考 https://man7.org/linux/man-pages/man2/linkat.2.html
pub fn sys_linkat(
    oldfd: isize,
    oldpath: *const u8,
    newfd: isize,
    newpath: *const u8,
    flags: u32,
) -> SyscallRet {
    // 保留所有不在允许集合中的位；只要调用方传入未知 flag，linkat(2) 就返回 EINVAL。
    if flags & !LINKAT_VALID_FLAGS != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

    let old_path_str = read_user_cstr(&memory_set, oldpath)?;
    let new_path_str = read_user_cstr(&memory_set, newpath)?;

    check_path_argument(
        &old_path_str,
        flags & AT_EMPTY_PATH as u32 != 0 && old_path_str.is_empty(),
        false,
    )?;
    check_path_argument(&new_path_str, false, true)?;

    // 处理 AT_EMPTY_PATH：若 oldpath 为空字符串，则使用 oldfd 对应的已打开文件
    if flags & AT_EMPTY_PATH as u32 != 0 && old_path_str.is_empty() {
        let new_abs_path = resolve_linkat_path(proc, newfd, &new_path_str)?;
        check_link_mounts(&new_abs_path, &new_abs_path)?;
        check_parent_permission(parent_path_of(&new_abs_path)?, true)?;
        // 新路径不能已存在
        if open(&new_abs_path, OpenFlags::empty(), NONE_MODE).is_ok() {
            return Err(SysErrNo::EEXIST);
        }
        // 通过 oldfd 获取原始 inode
        let old_file = proc.fd_table.get(oldfd as usize)?.file()?;
        let old_path = old_file.inode.path();
        check_hard_link_limit(&old_file.inode)?;
        old_file.inode.hard_link(&old_path, &new_abs_path)?;
        let inode = FsIndex::insert_inode_idx(&new_abs_path, old_file.inode.clone());
        cache_positive_dentry_path(&new_abs_path, inode);
        return Ok(0);
    }

    // 常规路径解析
    let old_abs_path = resolve_linkat_path(proc, oldfd, &old_path_str)?;
    let new_abs_path = resolve_linkat_path(proc, newfd, &new_path_str)?;
    if old_path_str.len() >= MAX_PATH_LEN || has_too_long_path_component(&old_path_str) {
        if has_self_referential_symlink_prefix(&old_abs_path) {
            return Err(SysErrNo::ELOOP);
        }
        if open(&old_abs_path, OpenFlags::empty(), NONE_MODE).err() == Some(SysErrNo::ELOOP) {
            return Err(SysErrNo::ELOOP);
        }
        return Err(SysErrNo::ENAMETOOLONG);
    }

    // LTP open14 links an O_TMPFILE fd through /proc/self/fd/<fd>. We do not
    // have a full procfs link implementation here, so materialize the current
    // fd contents into the destination path. Do this before validating the
    // procfs source parent: `/proc/self/fd` is a magic-link view rather than a
    // directory that exists in the root filesystem.
    if let Some(fd) = parse_proc_self_fd(&old_abs_path) {
        if flags & AT_SYMLINK_FOLLOW as u32 == 0 {
            return Err(SysErrNo::ELOOP);
        }
        // The anonymous file has no source pathname or mount entry. Checking
        // the target against itself still enforces a read-only destination.
        check_link_mounts(&new_abs_path, &new_abs_path)?;
        check_parent_permission(parent_path_of(&new_abs_path)?, true)?;
        let src = proc.fd_table.get(fd)?.any();
        // Creating the destination goes through the regular VFS path and may
        // re-enter process state, so release syscall-local process locks first.
        drop(memory_set);

        let stat = src.fstat();
        let dst = open(
            &new_abs_path,
            OpenFlags::O_CREATE | OpenFlags::O_EXCL | OpenFlags::O_RDWR,
            stat.st_mode & 0o7777,
        )?
        .file()?;
        let old_offset = src.lseek(0, SEEK_CUR)?;
        src.lseek(0, SEEK_SET)?;

        loop {
            let mut buf = vec![0u8; 4096];
            let read_len = src.read(unsafe { user_buffer_from_kernel(&mut buf) })?;
            if read_len == 0 {
                break;
            }
            let write_len = dst.write(unsafe { user_buffer_from_kernel(&mut buf[..read_len]) })?;
            if write_len != read_len {
                return Err(SysErrNo::EIO);
            }
        }

        src.lseek(old_offset as isize, SEEK_SET)?;
        let inode = FsIndex::insert_inode_idx(&new_abs_path, dst.inode.clone());
        cache_positive_dentry_path(&new_abs_path, inode);
        return Ok(0);
    }

    check_link_mounts(&old_abs_path, &new_abs_path)?;
    check_parent_permission(parent_path_of(&old_abs_path)?, false)?;
    check_parent_permission(parent_path_of(&new_abs_path)?, true)?;

    // 打开原文件
    let osfile = open(&old_abs_path, OpenFlags::empty(), NONE_MODE)?.file()?;

    // 不允许对目录创建硬链接
    if osfile.inode.types() == InodeType::Dir {
        return Err(SysErrNo::EPERM);
    }

    // 新路径不能已存在
    if open(&new_abs_path, OpenFlags::empty(), NONE_MODE).is_ok() {
        return Err(SysErrNo::EEXIST);
    }

    check_hard_link_limit(&osfile.inode)?;

    // 在文件系统层面创建硬链接
    osfile.inode.hard_link(&old_abs_path, &new_abs_path)?;
    // 更新目录索引：新路径与旧路径共享同一个 inode
    let inode = FsIndex::insert_inode_idx(&new_abs_path, osfile.inode.clone());
    cache_positive_dentry_path(&new_abs_path, inode);

    Ok(0)
}
