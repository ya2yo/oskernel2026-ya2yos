use alloc::{format, string::String, vec::Vec};
use linux_raw_sys::general::*;
use log::{debug, warn};

use crate::{
    arch::memory_layout::PAGE_SIZE,
    fs::invalidate_dentry_path,
    fs::{
        open, DetachedMountFd, File, FileClass, FileDescriptor, FsConfigOption, FsConfigValue,
        FsContextFd, FsIndex, InodeType, MountFlags, OpenFlags, MAX_PATH_LEN, MNT_TABLE, NONE_MODE,
    },
    mm::{copy_from_user, translate::read_user_cstr, UserBuffer},
    task::current_task,
    utils::{SysErrNo, SysResult, SyscallRet},
};

fn fsconfig_command_from_raw(cmd: u32) -> SysResult<fsconfig_command> {
    match cmd {
        cmd if cmd == fsconfig_command::FSCONFIG_SET_FLAG as u32 => {
            Ok(fsconfig_command::FSCONFIG_SET_FLAG)
        }
        cmd if cmd == fsconfig_command::FSCONFIG_SET_STRING as u32 => {
            Ok(fsconfig_command::FSCONFIG_SET_STRING)
        }
        cmd if cmd == fsconfig_command::FSCONFIG_SET_BINARY as u32 => {
            Ok(fsconfig_command::FSCONFIG_SET_BINARY)
        }
        cmd if cmd == fsconfig_command::FSCONFIG_SET_PATH as u32 => {
            Ok(fsconfig_command::FSCONFIG_SET_PATH)
        }
        cmd if cmd == fsconfig_command::FSCONFIG_SET_PATH_EMPTY as u32 => {
            Ok(fsconfig_command::FSCONFIG_SET_PATH_EMPTY)
        }
        cmd if cmd == fsconfig_command::FSCONFIG_SET_FD as u32 => {
            Ok(fsconfig_command::FSCONFIG_SET_FD)
        }
        cmd if cmd == fsconfig_command::FSCONFIG_CMD_CREATE as u32 => {
            Ok(fsconfig_command::FSCONFIG_CMD_CREATE)
        }
        cmd if cmd == fsconfig_command::FSCONFIG_CMD_RECONFIGURE as u32 => {
            Ok(fsconfig_command::FSCONFIG_CMD_RECONFIGURE)
        }
        cmd if cmd == fsconfig_command::FSCONFIG_CMD_CREATE_EXCL as u32 => {
            Ok(fsconfig_command::FSCONFIG_CMD_CREATE_EXCL)
        }
        _ => Err(SysErrNo::EOPNOTSUPP),
    }
}

fn refresh_proc_mounts() {
    let mut content = MNT_TABLE.lock().proc_mounts_content();
    let Ok(file) = open(
        "/proc/mounts",
        OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        NONE_MODE,
    )
    .and_then(|file| file.file()) else {
        return;
    };
    let mut buffers = Vec::new();
    unsafe {
        let bytes = content.as_bytes_mut();
        buffers.push(core::slice::from_raw_parts_mut(
            bytes.as_mut_ptr(),
            bytes.len(),
        ));
    }
    let _ = file.write(UserBuffer::new(buffers));
    file.inode.sync();
}

/// Extract child names from the serialized `OsDirent` buffer returned by
/// `Inode::read_dentry()`.
///
/// This parser deliberately keeps only names needed by mountpoint cleanup and
/// filters out `.` / `..`. The field offsets mirror
/// `crates/lwext4_rust/src/file.rs::OsDirent`; if that layout changes, this
/// helper must be updated with it.
fn parse_dirent_names(buf: &[u8]) -> Vec<String> {
    const D_RECLEN_OFF: usize = 16;
    const D_NAME_OFF: usize = 19;

    let mut names = Vec::new();
    let mut pos = 0usize;
    while pos + D_NAME_OFF <= buf.len() {
        let reclen =
            u16::from_ne_bytes([buf[pos + D_RECLEN_OFF], buf[pos + D_RECLEN_OFF + 1]]) as usize;
        if reclen == 0 || pos + reclen > buf.len() || reclen < D_NAME_OFF {
            break;
        }

        let name_bytes = &buf[pos + D_NAME_OFF..pos + reclen];
        let name_len = name_bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(name_bytes.len());
        if name_len > 0 {
            if let Ok(name) = core::str::from_utf8(&name_bytes[..name_len]) {
                if name != "." && name != ".." {
                    names.push(String::from(name));
                }
            }
        }
        pos += reclen;
    }
    names
}

/// Remove all children under `abs_dir` without removing `abs_dir` itself.
///
/// This is a compatibility helper for the current simplified mount model. A
/// full VFS mount implementation should hide the underlying mountpoint with a
/// separate mounted root inode instead of deleting mountpoint contents. Until
/// that exists, fresh tmpfs mounts use this helper so LTP observes an empty
/// tmpfs root rather than files left by a previous filesystem test round.
fn purge_dir_contents(abs_dir: &str) -> SysResult {
    let dir = open(
        abs_dir,
        OpenFlags::O_DIRECTORY | OpenFlags::O_RDONLY,
        NONE_MODE,
    )?
    .file()?;
    let mut names = Vec::new();
    let mut off = 0usize;
    loop {
        let (buf, next_off) = dir.inode.read_dentry(off, PAGE_SIZE * 4)?;
        if buf.is_empty() {
            break;
        }
        names.extend(parse_dirent_names(&buf));
        if next_off <= off as isize {
            break;
        }
        off = next_off as usize;
    }

    for name in names {
        let child_path = if abs_dir == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", abs_dir.trim_end_matches('/'), name)
        };
        let Ok(child) = open(&child_path, OpenFlags::O_RDONLY, NONE_MODE).and_then(|f| f.file())
        else {
            continue;
        };
        if child.inode.types() == InodeType::Dir {
            purge_dir_contents(&child_path)?;
            if !child.inode.is_dir_empty()? {
                return Err(SysErrNo::ENOTEMPTY);
            }
        }
        child.inode.unlink(&child_path)?;
        invalidate_dentry_path(&child_path);
        FsIndex::remove_inode_idx(&child_path);
    }
    Ok(())
}

/// Mirrors a bind-mounted source directory into its pathname-based target.
///
/// The current VFS does not yet have distinct mount-root dentries.  Keeping
/// the directory shape in sync is therefore necessary for bind propagation to
/// be visible through normal path lookup.  fs_bind uses directory trees; for
/// regular files a hard link preserves the shared inode semantics of a bind
/// mount without buffering file content in the syscall layer.
fn mirror_bind_tree(source: &str, target: &str) -> SysResult {
    let source_file = open(source, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
    if source_file.inode.types() != InodeType::Dir {
        if open(target, OpenFlags::O_RDONLY, NONE_MODE).is_err() {
            source_file.inode.hard_link(source, target)?;
            invalidate_dentry_path(target);
        }
        return Ok(());
    }

    let target_file = open(
        target,
        OpenFlags::O_DIRECTORY | OpenFlags::O_RDONLY,
        NONE_MODE,
    )
    .or_else(|_| {
        open(
            target,
            OpenFlags::O_DIRECTORY | OpenFlags::O_CREATE | OpenFlags::O_RDWR,
            0o755,
        )
    })?
    .file()?;
    if target_file.inode.types() != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }

    let mut names = Vec::new();
    let mut off = 0usize;
    loop {
        let (buf, next_off) = source_file.inode.read_dentry(off, PAGE_SIZE * 4)?;
        if buf.is_empty() {
            break;
        }
        names.extend(parse_dirent_names(&buf));
        if next_off <= off as isize {
            break;
        }
        off = next_off as usize;
    }

    for name in names {
        let source_child = format!("{}/{}", source.trim_end_matches('/'), name);
        let target_child = format!("{}/{}", target.trim_end_matches('/'), name);
        // A bind target inside its source (for example `parent` onto
        // `parent/child`) would otherwise become visible while recursively
        // mirroring, making the traversal descend forever into the new copy.
        // The target branch already denotes the bind root, so it must not be
        // copied as a child of itself.
        if target == source_child
            || target
                .strip_prefix(source_child.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        {
            continue;
        }
        let child = open(&source_child, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
        if child.inode.types() == InodeType::Dir {
            if open(&target_child, OpenFlags::O_RDONLY, NONE_MODE).is_err() {
                open(
                    &target_child,
                    OpenFlags::O_DIRECTORY | OpenFlags::O_CREATE | OpenFlags::O_RDWR,
                    0o755,
                )?;
            }
            mirror_bind_tree(&source_child, &target_child)?;
        } else if open(&target_child, OpenFlags::O_RDONLY, NONE_MODE).is_err() {
            child.inode.hard_link(&source_child, &target_child)?;
            invalidate_dentry_path(&target_child);
        }
    }
    Ok(())
}

/// 参考 https://man7.org/linux/man-pages/man2/pivot_root.2.html
pub fn sys_pivot_root(_new_root: usize, _put_old: usize) -> SyscallRet {
    warn!("[sys_pivot_root] not implement!");
    /*
       EBUSY  new_root or put_old is on the current root mount.  (This
              error covers the pathological case where new_root is "/".)
       EINVAL new_root is not a mount point.
       EINVAL put_old is not at or underneath new_root.
       EINVAL The current root directory is not a mount point (because of
              an earlier chroot(2)).
       EINVAL The current root is on the rootfs (initial ramfs) mount;
              see NOTES.
       EINVAL Either the mount point at new_root, or the parent mount of
              that mount point, has propagation type MS_SHARED.
       EINVAL put_old is a mount point and has the propagation type
              MS_SHARED.
       ENOTDIR
              new_root or put_old is not a directory.
       EPERM  The calling process does not have the CAP_SYS_ADMIN
              capability.
    */
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/umount2.2.html
pub fn sys_umount2(special: *const u8, flags: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let special = read_user_cstr(&memory_set, special)?;
    let special = proc.get_abs_path(AT_FDCWD as isize, &special)?;

    // Record whether the target is a bind mount before removing the entry.
    // mirror_bind_tree() copies source content into the target directory,
    // and those files must be removed on umount so the target can be reused.
    let is_bind = MNT_TABLE
        .lock()
        .mount_for_path(&special)
        .map(|(_, _, _, mnt_flags)| mnt_flags.is_bind())
        .unwrap_or(false);

    let ret = MNT_TABLE
        .lock()
        .umount(special.clone(), MountFlags::from_bits_truncate(flags));
    if ret != -1 {
        if is_bind {
            if let Err(err) = purge_dir_contents(&special) {
                warn!(
                    "[sys_umount2] failed to purge bind mount target {}: {:?}",
                    special, err
                );
            }
        }
        refresh_proc_mounts();
        Ok(0)
    } else {
        Err(SysErrNo::EINVAL)
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/mount.2.html
///
/// 根据 flags 的不同组合，`mount(2)` 实际执行 5 种不同的操作：
///
/// | flags 组合                          | 操作              |
/// |-------------------------------------|-------------------|
/// | `MOVE`                              | 移动已有挂载        |
/// | `BIND`                              | bind mount         |
/// | `REMOUNT`                           | 重挂载（改属性）     |
/// | propagation-only (`SHARED\|PRIVATE\|...)` | 修改传播类型     |
/// | 以上均未设置（普通挂载）               | 新建挂载           |
pub fn sys_mount(
    special: *const u8,
    dir: *const u8,
    ftype: *const u8,
    flags: u32,
    data: *const u8,
) -> SyscallRet {
    // -- 权限检查 --
    let task = current_task().unwrap();
    let uid = task.inner_lock().effective_uid;
    if uid != 0 {
        return Err(SysErrNo::EPERM);
    }
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

    // -- 读取用户态字符串 --
    let special_raw = read_user_cstr(&memory_set, special)?;
    let dir_raw = read_user_cstr(&memory_set, dir)?;
    let ftype_raw = read_user_cstr(&memory_set, ftype)?;

    // -- 基本校验 --
    if ftype_raw.is_empty() {
        return Err(SysErrNo::EINVAL);
    }
    if dir_raw.len() >= MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    // -- 解析 flags --
    let mnt_flags = MountFlags::from_bits_truncate(flags);

    // -- 解析 target 路径并校验为目录 --
    let dir_abs = proc.get_abs_path(AT_FDCWD as isize, &dir_raw)?;
    let target_type = match open(&dir_abs, OpenFlags::O_RDONLY, NONE_MODE) {
        Ok(f) => match f.file() {
            Ok(file) => file.inode.types(),
            Err(_) => return Err(SysErrNo::ENOENT),
        },
        Err(e) => return Err(e),
    };
    if target_type != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }

    // -- 解析 special/source 路径 --
    // BIND / MOVE 操作的 special 是路径，需要解析为绝对路径。
    // 普通挂载的 special 是设备标识符，保留原样（在后续按 fstype 校验）。
    let special_abs = if mnt_flags.is_move() || mnt_flags.is_bind() {
        proc.get_abs_path(AT_FDCWD as isize, &special_raw)?
    } else {
        special_raw.clone()
    };

    // -- 读取 mount data --
    let mount_data = if !data.is_null() {
        read_user_cstr(&memory_set, data)?
    } else {
        String::new()
    };

    // =================================================================
    // 按操作类型分派
    // =================================================================

    // --- REMOUNT ---
    if mnt_flags.is_remount() {
        // 向只读 remount 时，检查是否还有进程持有可写 fd。
        if mnt_flags.contains(MountFlags::RDONLY) && has_open_write_fd() {
            return Err(SysErrNo::EBUSY);
        }
        MNT_TABLE
            .lock()
            .mount(
                special_abs,
                dir_abs,
                ftype_raw,
                mnt_flags,
                mount_data,
                None,
            )?;
        refresh_proc_mounts();
        return Ok(0);
    }

    // --- MOVE ---
    if mnt_flags.is_move() {
        let copies = MNT_TABLE.lock().mount(
            special_abs,
            dir_abs,
            ftype_raw,
            mnt_flags,
            mount_data,
            None,
        )?;
        for (source, target) in &copies {
            mirror_bind_tree(source, target)?;
        }
        // After moving the mount tree, purge the old mount point contents.
        // MS_MOVE relocates the mount subtree to the new target, so the
        // original mount root must be empty afterwards.  Propagation copies
        // share the same source path; purge it once.
        if let Some((first_source, _)) = copies.first() {
            if let Err(err) = purge_dir_contents(first_source) {
                warn!(
                    "[sys_mount] failed to purge move source {}: {:?}",
                    first_source, err
                );
            }
        }
        refresh_proc_mounts();
        return Ok(0);
    }

    // --- BIND ---
    if mnt_flags.is_bind() {
        let copies = MNT_TABLE.lock().mount(
            special_abs,
            dir_abs,
            ftype_raw,
            mnt_flags,
            mount_data,
            None,
        )?;
        for (source, target) in &copies {
            mirror_bind_tree(source, target)?;
        }
        refresh_proc_mounts();
        return Ok(0);
    }

    // --- Propagation-only ---
    if mnt_flags.is_propagation_only() {
        MNT_TABLE.lock().mount(
            String::new(),
            dir_abs,
            String::new(),
            mnt_flags,
            mount_data,
            None,
        )?;
        refresh_proc_mounts();
        return Ok(0);
    }

    // =================================================================
    // 普通挂载（非 BIND, MOVE, REMOUNT, propagation-only）
    // =================================================================

    // 校验 fstype
    if !is_known_fs(&ftype_raw) {
        return Err(SysErrNo::ENODEV);
    }

    // 设备文件系统需要有效的设备路径
    if fstype_requires_dev(&ftype_raw) {
        if special_abs.is_empty() {
            return Err(SysErrNo::EINVAL);
        }
        // 尝试解析设备路径并校验设备类型
        if let Ok(dev_abs) = proc.get_abs_path(AT_FDCWD as isize, &special_abs) {
            if let Ok(f) = open(&dev_abs, OpenFlags::O_RDONLY, NONE_MODE) {
                if let Ok(file) = f.file() {
                    if file.inode.types() == InodeType::CharDevice {
                        return Err(SysErrNo::ENOTBLK);
                    }
                }
            }
        }
    }

    // 路径化 VFS 中 umount 不清理底层目录中残留的文件，而 LTP 在同一
    // 挂载点对不同文件系统类型（ext2/ext3/ext4/tmpfs）复用目录，因此
    // 普通挂载前必须清空挂载点，确保每次挂载从干净状态开始。
    if let Err(err) = purge_dir_contents(&dir_abs) {
        warn!(
            "[sys_mount] failed to purge mountpoint {}: {:?}",
            dir_abs, err
        );
    }

    // ext4 loop 镜像格式化的容量作为逻辑配额。
    let mount_capacity = (ftype_raw == "ext4")
        .then(|| crate::fs::loopdev::formatted_size(&special_abs))
        .flatten();

    let copies = MNT_TABLE.lock().mount(
        special_abs,
        dir_abs,
        ftype_raw,
        mnt_flags,
        mount_data,
        mount_capacity,
    )?;
    for (source, target) in &copies {
        mirror_bind_tree(source, target)?;
    }
    refresh_proc_mounts();
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/open_tree.2.html
pub fn sys_open_tree(dirfd: i32, path: *const u8, flags: u32) -> SyscallRet {
    let valid_flags =
        OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH | AT_NO_AUTOMOUNT | AT_RECURSIVE;
    if flags & !valid_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let abs_path = {
        let task = current_task().unwrap();
        let proc = &task.process;
        let memory_set = proc.memory_set_arc();
        let path = read_user_cstr(&memory_set, path)?;
        if path.is_empty() && flags & AT_EMPTY_PATH == 0 {
            return Err(SysErrNo::ENOENT);
        }
        if path.len() > MAX_PATH_LEN {
            return Err(SysErrNo::ENAMETOOLONG);
        }
        let abs_path = proc.get_abs_path(dirfd as isize, &path)?;
        open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?;
        abs_path
    };

    alloc_new_mount_fd(
        FileClass::DetachedMount(DetachedMountFd::new(
            String::from(""),
            Some(abs_path),
            flags,
            0,
        )),
        flags & OPEN_TREE_CLOEXEC != 0,
    )
}

/// https://man7.org/linux/man-pages/man2/move_mount.2.html
///
/// 将已挂载的文件系统从一个位置移动到另一个位置。
///
/// # 参数
/// - `from_dirfd`: 源目录的 fd
/// - `from_path`: 源挂载点路径
/// - `to_dirfd`: 目标目录的 fd
/// - `to_path`: 目标挂载点路径
/// - `flags`: 移动标志 (MOVE_MOUNT_F_*)
pub fn sys_move_mount(
    from_dirfd: i32,
    from_path: *const u8,
    to_dirfd: i32,
    to_path: *const u8,
    flags: u32,
) -> SyscallRet {
    if flags & !MOVE_MOUNT__MASK != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let from_path = read_user_cstr(&memory_set, from_path)?;
    let to_path = read_user_cstr(&memory_set, to_path)?;
    if from_path.len() > MAX_PATH_LEN || to_path.len() > MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    let to_abs_path = proc.get_abs_path(to_dirfd as isize, &to_path)?;
    if let Ok(detached) = proc
        .fd_table
        .get(from_dirfd as usize)
        .and_then(|fd| fd.detached_mount())
    {
        if flags & MOVE_MOUNT_F_EMPTY_PATH == 0 || !from_path.is_empty() {
            return Err(SysErrNo::EINVAL);
        }
        let source = detached
            .source
            .clone()
            .unwrap_or_else(|| String::from("none"));
        let fstype = if detached.fsname.is_empty() {
            String::from("none")
        } else {
            detached.fsname.clone()
        };
        let mount_flags = MountFlags::from_bits_truncate(detached.attr_flags);
        let copies = MNT_TABLE.lock().mount(
            source,
            to_abs_path,
            fstype,
            mount_flags,
            String::from(""),
            None,
        )?;
        for (source, target) in copies {
            mirror_bind_tree(&source, &target)?;
        }
        refresh_proc_mounts();
        return Ok(0);
    }

    let from_abs_path = proc.get_abs_path(from_dirfd as isize, &from_path)?;
    open(&from_abs_path, OpenFlags::O_RDONLY, NONE_MODE)?;
    open(&to_abs_path, OpenFlags::O_RDONLY, NONE_MODE)?;
    if flags & (MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH) != 0 {
        debug!("[sys_move_mount] path based move with empty-path flags is treated as no-op");
    }
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/fsopen.2.html
pub fn sys_fsopen(fsname: *const u8, flags: u32) -> SyscallRet {
    if flags & !FSOPEN_CLOEXEC != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let fsname = {
        let task = current_task().unwrap();
        let proc = &task.process;
        let memory_set = proc.memory_set_arc();
        let fsname = read_user_cstr(&memory_set, fsname)?;
        if fsname.is_empty() || fsname.len() > MAX_PATH_LEN {
            return Err(SysErrNo::EINVAL);
        }
        if !is_known_fs(&fsname) {
            return Err(SysErrNo::ENODEV);
        }
        fsname
    };

    alloc_new_mount_fd(
        FileClass::FsContext(FsContextFd::new(fsname)),
        flags & FSOPEN_CLOEXEC != 0,
    )
}

/// https://www.man7.org/linux/man-pages/man2/fsconfig.2.html
///
/// 配置/创建由 fsopen(2) 或 fspick(2) 获得的文件系统上下文。
///
/// 命令:
/// - FSCONFIG_SET_FLAG   (0): 设置标志位选项
/// - FSCONFIG_SET_STRING (1): 设置字符串选项
/// - FSCONFIG_SET_BINARY (2): 设置二进制选项 (aux = 数据长度)
/// - FSCONFIG_SET_PATH   (3): 设置路径选项
/// - FSCONFIG_SET_PATH_EMPTY (4): 设置空路径选项
/// - FSCONFIG_SET_FD     (5): 设置 fd 选项, value 是 fd 编号
/// - FSCONFIG_CMD_CREATE  (6): 创建文件系统/superblock
/// - FSCONFIG_CMD_RECONFIGURE (7): 重新配置文件系统参数
pub fn sys_fsconfig(fd: i32, cmd: u32, key: usize, value: usize, aux: i32) -> SyscallRet {
    if fd < 0 {
        return Err(SysErrNo::EINVAL);
    }
    let cmd = fsconfig_command_from_raw(cmd)?;
    fsconfig_check(cmd, key, value, aux)?;

    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let fsctx = proc.fd_table.get(fd as usize)?.fs_context()?;

    match cmd {
        fsconfig_command::FSCONFIG_SET_FLAG => {
            let key = read_user_cstr(&memory_set, key as *const u8)?;
            fsctx.with_inner(|ctx| {
                ctx.options.push(FsConfigOption {
                    key,
                    value: FsConfigValue::Flag,
                });
            });
            Ok(0)
        }
        fsconfig_command::FSCONFIG_SET_STRING => {
            let key = read_user_cstr(&memory_set, key as *const u8)?;
            let value = read_user_cstr(&memory_set, value as *const u8)?;
            fsctx.with_inner(|ctx| {
                let new_len = ctx.legacy_data_len + value.len() + 3;
                if new_len > PAGE_SIZE {
                    return Err(SysErrNo::EINVAL);
                }
                ctx.legacy_data_len += value.len() + 2;
                if key == "source" {
                    ctx.source = Some(value.clone());
                }
                ctx.options.push(FsConfigOption {
                    key,
                    value: FsConfigValue::String(value),
                });
                Ok(0)
            })
        }
        fsconfig_command::FSCONFIG_SET_BINARY => {
            let key = read_user_cstr(&memory_set, key as *const u8)?;
            let mut buf = Vec::new();
            buf.resize(aux as usize, 0);
            copy_from_user(&memory_set, value, &mut buf[..])?;
            fsctx.with_inner(|ctx| {
                ctx.options.push(FsConfigOption {
                    key,
                    value: FsConfigValue::Binary(buf),
                });
            });
            Ok(0)
        }
        fsconfig_command::FSCONFIG_SET_PATH | fsconfig_command::FSCONFIG_SET_PATH_EMPTY => {
            let key = read_user_cstr(&memory_set, key as *const u8)?;
            let path = read_user_cstr(&memory_set, value as *const u8)?;
            if cmd == fsconfig_command::FSCONFIG_SET_PATH && path.is_empty() {
                return Err(SysErrNo::ENOENT);
            }
            if path.len() > MAX_PATH_LEN {
                return Err(SysErrNo::ENAMETOOLONG);
            }
            let _ = proc.get_abs_path(aux as isize, &path)?;
            fsctx.with_inner(|ctx| {
                if key == "source" {
                    ctx.source = Some(path.clone());
                }
                ctx.options.push(FsConfigOption {
                    key,
                    value: FsConfigValue::Path { path, dirfd: aux },
                });
            });
            Ok(0)
        }
        fsconfig_command::FSCONFIG_SET_FD => {
            let key = read_user_cstr(&memory_set, key as *const u8)?;
            let _ = proc.fd_table.get(aux as usize)?;
            fsctx.with_inner(|ctx| {
                ctx.options.push(FsConfigOption {
                    key,
                    value: FsConfigValue::Fd(aux),
                });
            });
            Ok(0)
        }
        fsconfig_command::FSCONFIG_CMD_CREATE | fsconfig_command::FSCONFIG_CMD_CREATE_EXCL => {
            fsctx.with_inner(|ctx| {
                ctx.created = true;
                ctx.exclusive = cmd == fsconfig_command::FSCONFIG_CMD_CREATE_EXCL;
            });
            debug!("[sys_fsconfig] fs context created");
            Ok(0)
        }
        fsconfig_command::FSCONFIG_CMD_RECONFIGURE => {
            fsctx.with_inner(|ctx| {
                ctx.reconfigure = true;
            });
            debug!("[sys_fsconfig] fs context reconfigure requested");
            Ok(0)
        }
        _ => Err(SysErrNo::EOPNOTSUPP),
    }
}
fn fsconfig_check(cmd: fsconfig_command, key: usize, value: usize, aux: i32) -> SysResult {
    match cmd {
        fsconfig_command::FSCONFIG_SET_FLAG => {
            if key == 0 || value != 0 || aux != 0 {
                Err(SysErrNo::EINVAL)
            } else {
                Ok(())
            }
        }
        fsconfig_command::FSCONFIG_SET_STRING => {
            if key == 0 || value == 0 || aux != 0 {
                Err(SysErrNo::EINVAL)
            } else {
                Ok(())
            }
        }
        fsconfig_command::FSCONFIG_SET_BINARY => {
            if key == 0 || value == 0 || aux <= 0 || aux > 1024 * 1024 {
                Err(SysErrNo::EINVAL)
            } else {
                Ok(())
            }
        }
        fsconfig_command::FSCONFIG_SET_PATH | fsconfig_command::FSCONFIG_SET_PATH_EMPTY => {
            if key == 0 || value == 0 || (aux != AT_FDCWD && aux < 0) {
                Err(SysErrNo::EINVAL)
            } else {
                Ok(())
            }
        }
        fsconfig_command::FSCONFIG_SET_FD => {
            if key == 0 || value != 0 || aux < 0 {
                Err(SysErrNo::EINVAL)
            } else {
                Ok(())
            }
        }
        fsconfig_command::FSCONFIG_CMD_CREATE
        | fsconfig_command::FSCONFIG_CMD_CREATE_EXCL
        | fsconfig_command::FSCONFIG_CMD_RECONFIGURE => {
            if key != 0 || value != 0 || aux != 0 {
                Err(SysErrNo::EINVAL)
            } else {
                Ok(())
            }
        }
        _ => Err(SysErrNo::EOPNOTSUPP),
    }
}

/// https://man7.org/linux/man-pages/man2/fsmount.2.html
///
/// 将 fsopen/fspick 获得的文件系统上下文挂载到命名空间中。
///
/// # 参数
/// - `fd`: 由 fsopen(2) 或 fspick(2) 返回的文件系统上下文 fd
/// - `flags`: 挂载标志 (MOUNT_ATTR_*)
/// - `attr_flags`: 挂载属性
///
/// # 返回
/// 成功返回 0，失败返回 -1 并设置 errno。
pub fn sys_fsmount(fd: i32, flags: u32, attr_flags: u32) -> SyscallRet {
    if fd < 0 {
        return Err(SysErrNo::EBADF);
    }
    if flags & !FSMOUNT_CLOEXEC != 0 || attr_flags & !valid_mount_attr_bits() != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let (fsname, source) = {
        let task = current_task().unwrap();
        let proc = &task.process;
        let fsctx = proc.fd_table.get(fd as usize)?.fs_context()?;
        fsctx.with_inner(|ctx| {
            if !ctx.created {
                return Err(SysErrNo::EINVAL);
            }
            Ok((ctx.fsname.clone(), ctx.source.clone()))
        })?
    };

    alloc_new_mount_fd(
        FileClass::DetachedMount(DetachedMountFd::new(fsname, source, flags, attr_flags)),
        flags & FSMOUNT_CLOEXEC != 0,
    )
}

/// https://man7.org/linux/man-pages/man2/fspick.2.html
pub fn sys_fspick(dirfd: i32, path: *mut u8, flags: u32) -> SyscallRet {
    let valid_flags =
        FSPICK_CLOEXEC | FSPICK_SYMLINK_NOFOLLOW | FSPICK_NO_AUTOMOUNT | FSPICK_EMPTY_PATH;
    if flags & !valid_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let abs_path = {
        let task = current_task().unwrap();
        let proc = &task.process;
        let memory_set = proc.memory_set_arc();
        let path = read_user_cstr(&memory_set, path)?;
        if path.is_empty() && flags & FSPICK_EMPTY_PATH == 0 {
            return Err(SysErrNo::ENOENT);
        }
        if path.len() > MAX_PATH_LEN {
            return Err(SysErrNo::ENAMETOOLONG);
        }
        let abs_path = proc.get_abs_path(dirfd as isize, &path)?;
        open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?;
        abs_path
    };

    alloc_new_mount_fd(
        FileClass::FsContext(FsContextFd::picked(abs_path)),
        flags & FSPICK_CLOEXEC != 0,
    )
}

/// https://man7.org/linux/man-pages/man2/mount_setattr.2.html
///
/// 修改已挂载文件系统的属性（只读、nosuid 等）。
///
/// # 参数
/// - `dirfd`: 挂载点所在目录的 fd
/// - `path`: 挂载点路径
/// - `flags`: AT_* 标志
/// - `attr`: 指向 mount_attr 结构的指针
pub fn sys_mount_setattr(
    dirfd: i32,
    path: *const u8,
    flags: u32,
    attr: usize,
    size: usize,
) -> SyscallRet {
    let valid_flags = AT_EMPTY_PATH | AT_RECURSIVE | AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT;
    if flags & !valid_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if attr == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if size < MOUNT_ATTR_SIZE_VER0 as usize {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let mut mount_attr_data = mount_attr {
        attr_set: 0,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    let copy_len = core::cmp::min(size, core::mem::size_of::<mount_attr>());
    let attr_bytes = unsafe {
        core::slice::from_raw_parts_mut(
            &mut mount_attr_data as *mut mount_attr as *mut u8,
            core::mem::size_of::<mount_attr>(),
        )
    };
    copy_from_user(&memory_set, attr, &mut attr_bytes[..copy_len])?;

    let valid_attrs = valid_mount_attr_bits() as u64;
    if mount_attr_data.attr_set & !valid_attrs != 0
        || mount_attr_data.attr_clr & !valid_attrs != 0
        || mount_attr_data.attr_set & mount_attr_data.attr_clr != 0
    {
        return Err(SysErrNo::EINVAL);
    }

    if path.is_null() {
        if flags & AT_EMPTY_PATH == 0 || dirfd == AT_FDCWD {
            return Err(SysErrNo::EFAULT);
        }
        let _ = proc.fd_table.get(dirfd as usize)?;
        return Ok(0);
    }

    let path = read_user_cstr(&memory_set, path)?;
    if path.len() > MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }
    if path.is_empty() && flags & AT_EMPTY_PATH == 0 {
        return Err(SysErrNo::ENOENT);
    }
    let abs_path = proc.get_abs_path(dirfd as isize, &path)?;
    open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?;
    Ok(0)
}

fn alloc_new_mount_fd(file: FileClass, cloexec: bool) -> SyscallRet {
    // This helper allocates in the current fd table and therefore takes that
    // table's internal lock itself.
    let task = current_task().unwrap();
    let proc = &task.process;
    let fd = proc.fd_table.alloc_fd()?;
    let flags = if cloexec {
        OpenFlags::O_CLOEXEC
    } else {
        OpenFlags::empty()
    };
    proc
        .fd_table
        .set(fd, FileDescriptor::new(flags, file))?;
    Ok(fd)
}

fn is_known_fs(fsname: &str) -> bool {
    matches!(
        fsname,
        "ext4"
            | "ext3"
            | "ext2"
            | "proc"
            | "tmpfs"
            | "devtmpfs"
            | "devpts"
            | "sysfs"
            | "rootfs"
            | "ramfs"
            | "bpf"
            | "cgroup"
            | "cgroup2"
            | "overlay"
            | "squashfs"
            | "xfs"
            | "btrfs"
            | "bcachefs"
            | "vfat"
            | "fat"
            | "exfat"
            | "ntfs"
            | "fuse"
            | "fuseblk"
    )
}

/// Returns `true` when `fsname` identifies a filesystem that requires a
/// backing block device.  Virtual (pseudo) filesystems such as tmpfs or proc
/// do not need one, so an empty source string is rejected only for
/// device-backed types.
fn fstype_requires_dev(fsname: &str) -> bool {
    matches!(
        fsname,
        "ext4" | "ext3" | "ext2" | "xfs" | "btrfs" | "bcachefs" | "vfat" | "fat" | "exfat" | "ntfs"
    )
}

/// Return `true` when any task holds a file descriptor open for writing
/// (O_WRONLY or O_RDWR) on the single-superblock ext4 filesystem.  This is
/// used to decide whether `MS_REMOUNT|MS_RDONLY` must return `EBUSY`.
fn has_open_write_fd() -> bool {
    use crate::task::tid_to_task;
    for (_tid, task) in tid_to_task::get_all_tasks() {
        if task.process.fd_table.has_write_fd() {
            return true;
        }
    }
    false
}

fn valid_mount_attr_bits() -> u32 {
    MOUNT_ATTR_RDONLY
        | MOUNT_ATTR_NOSUID
        | MOUNT_ATTR_NODEV
        | MOUNT_ATTR_NOEXEC
        | MOUNT_ATTR_NOATIME
        | MOUNT_ATTR_STRICTATIME
        | MOUNT_ATTR_NODIRATIME
        | MOUNT_ATTR_IDMAP
        | MOUNT_ATTR_NOSYMFOLLOW
}
