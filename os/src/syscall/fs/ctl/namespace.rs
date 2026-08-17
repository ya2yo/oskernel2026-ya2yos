use super::*;

/// 实现 `symlinkat(2)`，在指定目录下创建一个符号链接。
///
/// 该函数读取目标字符串和 linkpath，解析 linkpath 所在目录，拒绝覆盖已存在路径，
/// 然后调用父目录 inode 创建 symlink。创建后失效对应 dentry cache。
/// 参考 https://www.man7.org/linux/man-pages/man2/symlink.2.html
pub fn sys_symlinkat(target: *const u8, newdirfd: isize, linkpath: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = Arc::clone(&task.process);
    let memory_set = proc.memory_set_arc();
    let target_path = read_user_cstr(&memory_set, target)?;
    let link_path = read_user_cstr(&memory_set, linkpath)?;

    // debug!(
    //     "[sys_symlinkat] target is {},newdirfd is {},linkpath is {}",
    //     target_path, newdirfd, link_path
    // );

    let abs_link_path = proc.get_abs_path(newdirfd, &link_path)?;
    //检查linkpath是否已存在
    if let Ok(_) = open(&abs_link_path, OpenFlags::empty(), NONE_MODE) {
        return Err(SysErrNo::EEXIST);
    }

    let (abs_link_dir, _) = rsplit_once(abs_link_path.as_str(), "/");
    let new_file = open(
        &abs_link_dir,
        OpenFlags::O_DIRECTORY | OpenFlags::O_RDONLY,
        NONE_MODE,
    )?
    .file()?;
    new_file
        .inode
        .sym_link(target_path.as_str(), abs_link_path.as_str())?;
    invalidate_dentry_path(&abs_link_path);
    Ok(0)
}

/// 实现 `renameat2(2)` 的基础重命名路径。
///
/// 当前实现忽略高级 rename flags，把 old path 和 new path 解析后交给 inode 层执行
/// rename。成功后清理旧路径和新路径的 dentry/inode cache，避免后续访问命中过期
/// 路径别名。
/// 参考 https://man7.org/linux/man-pages/man2/renameat2.2.html
pub fn sys_renameat2(
    olddirfd: isize,
    oldpath: *const u8,
    newdirfd: isize,
    newpath: *const u8,
    _flags: u32,
) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let oldpath = read_user_cstr(&memory_set, oldpath)?;
    let newpath = read_user_cstr(&memory_set, newpath)?;

    let old_abs_path = proc.get_abs_path(olddirfd, &oldpath)?;
    // rename only needs the source inode. Opening a directory with write intent
    // is rejected by VFS before the inode rename operation can run.
    let osfile = open(&old_abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
    let new_abs_path = proc.get_abs_path(newdirfd, &newpath)?;
    let ret = osfile.inode.rename(&old_abs_path, &new_abs_path);
    // rename 成功后，旧路径的文件已移到新路径，需要更新/清理 FsIndex 缓存，
    // 否则后续对旧路径的访问会命中缓存中的过期 inode，导致 fstat 等操作
    // 因底层 ext4_stat_get 找不到原路径而返回 ENOENT → panic。
    if ret.is_ok() {
        invalidate_dentry_path(&old_abs_path);
        invalidate_dentry_path(&new_abs_path);
        if osfile.inode.types() == InodeType::Dir {
            // `lwext4` metadata APIs are pathname-based.  Moving a directory
            // must therefore retarget every cached descendant that may still
            // back an open fd, rather than only invalidating the directory.
            FsIndex::remap_subtree_paths(&old_abs_path, &new_abs_path);
        } else {
            FsIndex::remove_inode_idx(&old_abs_path);
            FsIndex::remove_inode_idx(&new_abs_path);
        }
    }
    ret
}

/// 检查目录权限：路径遍历需要 search；创建目录项时还需要 write。
pub(super) fn check_parent_permission(parent_path: &str, need_write: bool) -> SyscallRet {
    let parent = open(parent_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
    if parent.inode.types() != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }

    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let uid = task_inner.effective_uid;
    let gid = task_inner.effective_gid;
    drop(task_inner);
    if uid == 0 {
        return Ok(0);
    }

    let parent_stat = parent.inode.fstat();
    let parent_mode = FileMode::from_bits_truncate(parent.inode.fmode()? & 0xfff);
    let has_exec = mode_allows(
        parent_mode,
        &parent_stat,
        uid,
        gid,
        FileMode::S_IXUSR,
        FileMode::S_IXGRP,
        FileMode::S_IXOTH,
    );
    let has_write = !need_write
        || mode_allows(
            parent_mode,
            &parent_stat,
            uid,
            gid,
            FileMode::S_IWUSR,
            FileMode::S_IWGRP,
            FileMode::S_IWOTH,
        );
    if !has_exec || !has_write {
        return Err(SysErrNo::EACCES);
    }
    Ok(0)
}
