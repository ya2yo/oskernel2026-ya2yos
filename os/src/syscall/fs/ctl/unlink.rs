use super::*;

/// 实现 `unlinkat(2)`，删除普通目录项或按 `AT_REMOVEDIR` 删除空目录。
///
/// 该函数区分文件和目录错误码，目录删除前检查是否为空；普通文件若仍被 fd 持有且
/// link count 为 1，则标记延迟删除，等待最后一个 inode 引用释放后再由底层清理。
/// 成功删除或延迟删除后会失效 dentry 和 inode cache。
/// 参考 https://man7.org/linux/man-pages/man2/unlinkat.2.html
pub fn sys_unlinkat(dirfd: isize, path: *const u8, flags: u32) -> SyscallRet {
    if flags & !(AT_REMOVEDIR as u32) != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

    let path = read_user_cstr(&memory_set, path)?;
    check_path_argument(&path, false, true)?;
    let abs_path = proc.get_abs_path(dirfd, &path)?;
    // TODO(ZMY) 支持符号链接,socket,FIFO,device
    // 如果是File但尚有对应的fd未关闭,等到close时unlink
    // 如果是符号链接,直接移除
    // 如果是socket, FIFO, or device,移除但现有的fd可继续使用
    let osfile = open(
        &abs_path,
        OpenFlags::O_RDONLY | OpenFlags::O_UNLINK,
        NONE_MODE,
    )?
    .file()?;

    let is_dir = osfile.inode.types() == InodeType::Dir;
    let remove_dir = flags & (AT_REMOVEDIR as u32) != 0;
    if !is_dir && remove_dir {
        return Err(SysErrNo::ENOTDIR);
    }
    if is_dir && !remove_dir {
        return Err(SysErrNo::EISDIR);
    }
    if is_dir && !osfile.inode.is_dir_empty()? {
        return Err(SysErrNo::ENOTEMPTY);
    }
    if is_dir {
        osfile.inode.unlink(&abs_path)?;
        MNT_TABLE.lock().remove_file(&abs_path);
        invalidate_dentry_path(&abs_path);
        FsIndex::remove_inode_idx(&abs_path);
        return Ok(0);
    }

    let locked_fs_info = &proc.fs_info;

    // debug!(
    //     "[sys_unlinkat] path={},link_cnt={},has_activate_fd={}",
    //     &abs_path,
    //     osfile.inode.link_cnt()?,
    //     locked_fs_info.has_fd(&abs_path)
    // );
    // TODO: HXC: 我怀疑这里的has_fd是有问题的
    let has_fd = locked_fs_info.has_fd(&abs_path);
    if has_fd && osfile.inode.link_cnt()? == 1 {
        osfile.inode.delay();
        MNT_TABLE.lock().remove_file(&abs_path);
        invalidate_dentry_path(&abs_path);
        FsIndex::remove_inode_idx(&abs_path);
    } else {
        osfile.inode.unlink(&abs_path)?;
        MNT_TABLE.lock().remove_file(&abs_path);
        invalidate_dentry_path(&abs_path);
        FsIndex::remove_inode_idx(&abs_path);
    }

    Ok(0)
}
