use super::*;

/// 实现 `mknodat(2)`，在指定目录下创建 FIFO、设备、socket 或普通文件节点。
///
/// 该函数解析用户路径和 mode 类型位：普通文件复用 `open(O_CREAT|O_EXCL)` 路径以
/// 获得一致的权限和缓存行为；目录类型要求调用 `mkdirat(2)`；FIFO/设备/socket 等
/// 由底层 inode 创建后登记到 inode cache 和 special node 表。
/// 参考 https://www.man7.org/linux/man-pages/man2/mknod.2.html
pub fn sys_mknodat(dirfd: i32, path: usize, mode: usize, _dev: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let fd_table = proc.fd_table.clone();
    let path = read_user_cstr(&memory_set, path as *const u8)?;

    // AT_FDCWD = -100
    if dirfd != -100 && dirfd as usize >= fd_table.len() {
        return Err(SysErrNo::EBADF);
    }
    let abs_path = proc.get_abs_path(dirfd as isize, &path)?;
    drop(memory_set);
    // 已存在 → EEXIST
    if open(&abs_path, OpenFlags::O_RDWR, NONE_MODE).is_ok() {
        return Err(SysErrNo::EEXIST);
    }

    let perm = mode & 0o777;
    let type_mode = mode & 0o170000;
    let type_bits = (type_mode >> 12) as u8;

    let inode_type = match type_bits {
        0o1 => InodeType::Fifo,
        0o2 => InodeType::CharDevice,
        0o4 => InodeType::Dir,
        0o6 => InodeType::BlockDevice,
        0o10 => InodeType::File,
        0o12 => InodeType::SymLink,
        0o14 => InodeType::Socket,
        _ => return Err(SysErrNo::EINVAL),
    };

    // 普通文件走 open() 以获得完整权限检查
    if inode_type == InodeType::File {
        return match open(
            &abs_path,
            OpenFlags::O_CREATE | OpenFlags::O_EXCL | OpenFlags::O_RDWR,
            perm as u32,
        ) {
            Ok(_) => Ok(0),
            Err(_) => Err(SysErrNo::ENOENT),
        };
    }

    // 目录应使用 mkdirat
    if inode_type == InodeType::Dir {
        return Err(SysErrNo::EINVAL);
    }

    // FIFO / 设备 / 套接字：直接通过 inode 创建
    let root = superblock_root_inode();
    let inode = root.create(&abs_path, inode_type)?;
    inode.fmode_set((type_mode | perm) as u32)?;
    let inode = FsIndex::insert_inode_idx(&abs_path, inode);
    cache_positive_dentry_path(&abs_path, inode);
    FsIndex::insert_special_node_type(&abs_path, inode_type);
    Ok(0)
}

/// 实现 `mkdirat(2)`，按 dirfd 和用户路径创建目录。
///
/// 该函数负责读取用户路径、解析绝对路径、拒绝空路径和根目录重复创建，并最终通过
/// `open(O_CREATE|O_EXCL|O_DIRECTORY)` 走统一的目录创建路径。
/// 参考 https://man7.org/linux/man-pages/man2/mkdirat.2.html
pub fn sys_mkdirat(dirfd: isize, path: *const u8, mode: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let path = read_user_cstr(&memory_set, path)?;
    if path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    drop(memory_set);
    // debug!(
    //     "[sys_mkdirat] dirfd is {},path is {},mode is {}",
    //     dirfd, path, mode
    // );

    if path.len() >= MAX_PATH_LEN || has_too_long_path_component(&path) {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    if dirfd != -100 && dirfd as usize >= proc.fd_table.len() {
        return Err(SysErrNo::EBADF);
    }
    let abs_path = proc.get_abs_path(dirfd, &path)?;
    if abs_path.bytes().all(|b| b == b'/') {
        return Err(SysErrNo::EEXIST);
    }
    open(
        &abs_path,
        OpenFlags::O_RDWR | OpenFlags::O_CREATE | OpenFlags::O_EXCL | OpenFlags::O_DIRECTORY,
        mode,
    )?;
    Ok(0)
}

/// 实现 `getdents64(2)`，从目录 fd 读取目录项并拷贝到用户缓冲区。
///
/// 仅允许目录文件描述符；读取完成后把目录流 offset 更新为底层 `read_dentry`
/// 返回的新 cookie。`usize::MAX` 被用作 EOF cookie，此时直接返回 0。
/// 参考 https://man7.org/linux/man-pages/man2/getdents64.2.html
pub fn sys_getdents64(fd: usize, buf: *const u8, len: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = &*process.memory_set_arc();
    debug!(
        "[sys_getdents64] fd is {}, buf addr  is {:x}, len is {}",
        fd, buf as usize, len
    );

    let file = process.fd_table.get(fd)?.file()?;
    if !file.inode.types().is_dir() {
        return Err(SysErrNo::ENOTDIR);
    }
    if file.inode.path() == "/proc" {
        crate::fs::materialize_proc_dirs()?;
    }
    // read_dentry uses usize::MAX as the EOF cookie. It is not a byte offset,
    // so feeding it back into lseek(SEEK_CUR) would turn a clean EOF into EINVAL.
    let off = file.offset();
    if off == usize::MAX {
        return Ok(0);
    }
    let (de, off) = file.inode.read_dentry(off, len)?;
    copy_to_user(&memory_set, buf as usize, de.as_slice())?;
    file.set_offset(off as usize);
    return Ok(de.len());
}
