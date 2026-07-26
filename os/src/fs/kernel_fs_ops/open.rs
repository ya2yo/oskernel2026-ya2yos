use crate::fs::{
    is_dynamic_loader_path, map_library_path, DentryLookup, MountFlags, DENTRY_CACHE, MNT_TABLE,
    NONE_MODE,
};
use crate::syscall::{fs::file_lock, FileMode};
use crate::task::current_task;
use crate::utils::SysResult;

use super::*;
use alloc::sync::Arc;
use alloc::{format, string::String};
use linux_raw_sys::general::CAP_FOWNER;
use log::{debug, warn};

/// 将绝对路径拆分为父目录路径和末级名称。
///
/// 末尾斜杠会被忽略；根目录和空路径没有可创建或查找的末级名称，返回 `None`。
fn split_parent_child(abs_path: &str) -> Option<(&str, &str)> {
    let abs_path = abs_path.trim_end_matches('/');
    if abs_path.is_empty() || abs_path == "/" {
        return None;
    }

    let idx = abs_path.rfind('/').unwrap_or(0);
    let parent = if idx == 0 { "/" } else { &abs_path[..idx] };
    Some((parent, &abs_path[idx + 1..]))
}

/// 将父目录和末级名称拼接为规范的绝对路径。
///
/// 根目录需要特殊处理，以避免生成 `//<child>`。
fn join_parent_child(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{}", child)
    } else {
        format!("{}/{}", parent, child)
    }
}

/// 创建目标的已解析父目录及其对应路径信息。
struct ParentPath {
    parent_inode: Arc<dyn Inode>,
    create_path: String,
    child_name: String,
}

/// 解析创建目标的父目录，并构造由实际父 inode 路径派生的创建路径。
///
/// 父目录不存在时返回 `ENOENT`，父 inode 不是目录时返回 `ENOTDIR`。
fn resolve_parent_path(abs_path: &str) -> SysResult<ParentPath> {
    let Some((parent_path, child_name)) = split_parent_child(abs_path) else {
        return Err(SysErrNo::ENOENT);
    };

    let parent_inode =
        match FsIndex::find_inode_idx(parent_path).filter(|inode| inode.types().is_dir()) {
            Some(inode) => inode,
            None => {
                // O_NOFOLLOW/O_UNLINK callers may have cached a final symlink
                // (for example Debian's `/bin -> /usr/bin`).  A later path
                // component needs the resolved directory instead of that inode.
                let inode = superblock_root_inode().find(parent_path, OpenFlags::O_DIRECTORY, 0)?;
                FsIndex::insert_inode_idx(parent_path, inode)
            }
        };

    Ok(ParentPath {
        create_path: join_parent_child(&parent_inode.path(), child_name),
        parent_inode,
        child_name: String::from(child_name),
    })
}

/// 返回用于创建或重试查找的规范目标路径。
fn resolve_create_path(abs_path: &str) -> SysResult<String> {
    resolve_parent_path(abs_path).map(|target| target.create_path)
}

/// 校验 `O_NOATIME` 的 Linux 权限要求。
///
/// 仅文件所有者或持有 `CAP_FOWNER` 的任务可以禁止该 inode 的 atime 更新；
/// 内核早期没有当前任务时不施加该检查。
fn check_noatime_permission(inode: &Arc<dyn Inode>, flags: OpenFlags) -> SysResult {
    if !flags.contains(OpenFlags::O_NOATIME) {
        return Ok(());
    }

    let Some(task) = current_task() else {
        return Ok(());
    };
    let (euid, has_cap_fowner) = {
        let task_inner = task.inner_lock();
        let cap = CAP_FOWNER as usize;
        let word = cap / 32;
        let bit = cap % 32;
        (
            task_inner.effective_uid,
            word < task_inner.capabilities.effective.len()
                && (task_inner.capabilities.effective[word] & (1u32 << bit)) != 0,
        )
    };

    if euid == inode.fstat().st_uid || has_cap_fowner {
        Ok(())
    } else {
        Err(SysErrNo::EPERM)
    }
}

/// 使用已缓存的父目录执行一次末级目录项查找。
///
/// 返回 `None` 表示父目录未缓存，调用者应退回到根 inode 的完整路径查找。
/// `O_NOFOLLOW` 与内部 `O_UNLINK` 禁用目录项缓存，以保留末级符号链接本身的语义。
fn find_from_cached_parent(abs_path: &str, flags: OpenFlags) -> Option<SysResult<Arc<dyn Inode>>> {
    let (parent_path, child_name) = split_parent_child(abs_path)?;
    let parent_inode = FsIndex::find_inode_idx(parent_path)?;
    if !parent_inode.types().is_dir() {
        return Some(Err(SysErrNo::ENOTDIR));
    }

    let preserve_final_symlink = flags.intersects(OpenFlags::O_NOFOLLOW | OpenFlags::O_UNLINK);
    if !preserve_final_symlink {
        match DENTRY_CACHE.lookup(&parent_inode, child_name) {
            Some(DentryLookup::Positive(inode)) => return Some(Ok(inode)),
            Some(DentryLookup::Negative) if !flags.contains(OpenFlags::O_CREATE) => {
                return Some(Err(SysErrNo::ENOENT));
            }
            _ => {}
        }
    }

    let lookup_path = join_parent_child(&parent_inode.path(), child_name);
    let found = parent_inode.find(&lookup_path, flags, 0).map(|inode| {
        // `O_NOFOLLOW` and the internal `O_UNLINK` return the final symlink
        // itself.  That object must not populate the normal pathname cache:
        // a later ordinary open would reuse it and pass the link pathname to
        // lwext4, whose file-open API deliberately does not follow links.
        if preserve_final_symlink {
            return inode;
        }
        let inode = FsIndex::insert_inode_idx(&lookup_path, inode);
        if lookup_path != abs_path {
            FsIndex::insert_inode_idx(abs_path, inode.clone());
        }
        if !preserve_final_symlink {
            DENTRY_CACHE.insert_positive(&parent_inode, child_name, inode.clone());
        }
        inode
    });
    if found.as_ref().err() == Some(&SysErrNo::ENOENT)
        && !preserve_final_symlink
        && !flags.contains(OpenFlags::O_CREATE)
    {
        DENTRY_CACHE.insert_negative(&parent_inode, child_name);
    }
    Some(found)
}

/// 在父目录下写入新建或已确认存在的正目录项缓存。
fn cache_created_dentry(parent_inode: &Arc<dyn Inode>, child_name: &str, inode: Arc<dyn Inode>) {
    if !child_name.is_empty() {
        DENTRY_CACHE.insert_positive(parent_inode, child_name, inode);
    }
}

/// 使父目录下指定名称的目录项缓存失效。
fn invalidate_dentry(parent_inode: &Arc<dyn Inode>, child_name: &str) {
    if !child_name.is_empty() {
        DENTRY_CACHE.invalidate(parent_inode, child_name);
    }
}

/// 使绝对路径对应的末级目录项缓存失效。
///
/// 父目录尚未进入 inode 索引时无需处理，后续路径解析会从文件系统重新查找。
pub fn invalidate_dentry_path(abs_path: &str) {
    if let Some((parent_path, child_name)) = split_parent_child(abs_path) {
        if let Some(parent_inode) = FsIndex::find_inode_idx(parent_path) {
            invalidate_dentry(&parent_inode, child_name);
        }
    }
}

/// 将绝对路径对应 inode 写入其已缓存父目录的正目录项缓存。
///
/// 本函数不建立父目录 inode 索引，避免缓存更新路径隐式触发文件系统查找。
pub fn cache_positive_dentry_path(abs_path: &str, inode: Arc<dyn Inode>) {
    if let Some((parent_path, child_name)) = split_parent_child(abs_path) {
        if let Some(parent_inode) = FsIndex::find_inode_idx(parent_path) {
            cache_created_dentry(&parent_inode, child_name, inode);
        }
    }
}

/// 在 `abs_path` 创建节点，并返回与 `open(2)` 一致的文件对象。
///
/// 创建前检查父目录写和搜索权限，按进程 umask 修正权限位，并继承设有
/// `S_ISGID` 父目录的组 ID 与 setgid 位。成功后同步 inode 索引和目录项缓存。
fn create_file(abs_path: &str, flags: OpenFlags, mode: u32) -> SysResult<FileClass> {
    debug!(
        "[create_file] abs_path={}, flags={:?}, mode={:o}",
        abs_path, flags, mode
    );
    let target = resolve_parent_path(abs_path)?;
    let create_path = target.create_path;
    {
        let mnt_table = MNT_TABLE.lock();
        if let Some((_, _, _, mount_flags)) = mnt_table.mount_for_path(&create_path) {
            if mount_flags.contains(MountFlags::RDONLY) {
                return Err(SysErrNo::EROFS);
            }
        }
    }
    let parent_inode = target.parent_inode;
    let child_name = target.child_name;
    invalidate_dentry(&parent_inode, &child_name);
    let task_ids = current_task().map(|task| {
        let task_inner = task.inner_lock();
        (task_inner.effective_uid, task_inner.effective_gid)
    });
    let parent_stat = task_ids.map(|_| parent_inode.fstat());

    if let Some((my_uid, my_gid)) = task_ids {
        // root (euid 0) 绕过权限检查；Linux 文件权限检查基于 effective uid。
        if my_uid != 0 {
            let pstat = parent_stat.as_ref().ok_or(SysErrNo::EACCES)?;
            let parent_mode = FileMode::from_bits_truncate((pstat.st_mode & 0xfff) as u32);
            let owner_uid = pstat.st_uid;
            let owner_gid = pstat.st_gid;

            // 确定进程属于 owner / group / other 哪一类。
            let (has_write, has_exec) = if my_uid == owner_uid {
                (
                    parent_mode.contains(FileMode::S_IWUSR),
                    parent_mode.contains(FileMode::S_IXUSR),
                )
            } else if my_gid == owner_gid {
                (
                    parent_mode.contains(FileMode::S_IWGRP),
                    parent_mode.contains(FileMode::S_IXGRP),
                )
            } else {
                (
                    parent_mode.contains(FileMode::S_IWOTH),
                    parent_mode.contains(FileMode::S_IXOTH),
                )
            };

            if !has_exec {
                debug!("[create_file] EACCES: no exec permission on parent");
                return Err(SysErrNo::EACCES);
            }
            if !has_write {
                debug!("[create_file] EACCES: no write permission on parent");
                return Err(SysErrNo::EACCES);
            }
        } else {
            debug!("[create_file] root user, bypass permission check");
        }
    }

    let (readable, writable) = flags.read_write();
    let node_type = flags.node_type();
    let inode = parent_inode.create(&create_path, node_type)?;
    // Apply the process umask to the requested file mode.
    // umask specifies which permission bits to *clear* from the mode.
    // During early boot (fs::init) there is no current task, so we
    // fall back to the default umask 0o022.
    let umask = match current_task() {
        Some(task) => {
            let proc_inner = &task.process;
            proc_inner.fs_info.get_umask()
        }
        None => 0o022,
    };
    let mut effective_mode = mode & !umask;
    if node_type == InodeType::Dir {
        if let Some(parent_stat) = parent_stat.as_ref() {
            if parent_stat.st_mode & 0o2000 != 0 {
                effective_mode |= 0o2000;
            }
        }
    }
    // debug!(
    //     "[create_file] mode={:o} umask={:o} → effective={:o}",
    //     mode, umask, effective_mode
    // );
    inode.fmode_set(effective_mode)?;
    if let Some((uid, effective_gid)) = task_ids {
        // Linux assigns new inode gid from the parent directory when S_ISGID is set.
        let parent_stat = parent_stat.as_ref().ok_or(SysErrNo::EACCES)?;
        let parent_mode = parent_stat.st_mode & 0o7777;
        let gid = if parent_mode & 0o2000 != 0 {
            parent_stat.st_gid
        } else {
            effective_gid
        };
        inode.owner_set(uid, gid)?;
    }
    let inode = FsIndex::insert_inode_idx(&create_path, inode);
    if create_path != abs_path {
        FsIndex::insert_inode_idx(abs_path, inode.clone());
    }
    cache_created_dentry(&parent_inode, &child_name, inode.clone());
    let osinode = OSFile::new(
        readable,
        writable,
        flags.contains(OpenFlags::O_APPEND),
        inode,
    );
    Ok(FileClass::File(Arc::new(osinode)))
}
/// 实现打开文件的共享逻辑。
///
/// `map_dynamic` 为真时会映射动态库路径；普通用户态打开使用该模式，直接
/// 打开入口则保留调用方提供的路径。该函数同时处理 inode/目录项缓存、
/// 创建、`O_EXCL`、`O_DIRECTORY`、`O_NOATIME`、写权限、文件租约和 `O_TRUNC`。
fn open_inner(
    abs_path: &str,
    flags: OpenFlags,
    mode: u32,
    map_dynamic: bool,
) -> SysResult<FileClass> {
    debug!("open({},{:?},{})", abs_path, flags, mode);
    // log::info!("[open] abs_path={}", abs_path);
    // 如果是动态链接文件,转换路径
    // HXC: 转个锤子，本来输入的路径就是转换后的
    // HXC: 我错了还是得转，因为用户态也可能想要这个文件
    // 也许以后可以改成符号链接实现这种映射？
    debug!("abs_path is {}", abs_path);
    let mut abs_path: &str = abs_path;
    // If the mount has NOSYMFOLLOW, prevent symlink traversal by adding O_NOFOLLOW.
    // This must be done before the inode lookup so that find() rejects symlinks.
    //
    // Do not add O_NOFOLLOW when the caller already uses O_UNLINK (readlinkat,
    // unlinkat, faccessat with AT_SYMLINK_NOFOLLOW): those callers operate on the
    // symlink inode itself rather than following its target.  Adding O_NOFOLLOW
    // would turn their legitimate "look at the link" into an ELOOP rejection.
    let mut flags = flags;
    {
        let mnt_table = MNT_TABLE.lock();
        if let Some((_, _, _, mount_flags)) = mnt_table.mount_for_path(abs_path) {
            if mount_flags.contains(MountFlags::NOSYMFOLLOW) && !flags.contains(OpenFlags::O_UNLINK)
            {
                flags |= OpenFlags::O_NOFOLLOW;
            }
        }
    }
    if map_dynamic {
        // The linker can name its interpreter explicitly from a native libc
        // script. Prefer that matching loader; legacy images still fall back
        // to the compatibility target when the original path is absent.
        let native_loader_exists = is_dynamic_loader_path(abs_path)
            && open_direct(abs_path, OpenFlags::O_RDONLY, NONE_MODE).is_ok();
        if !native_loader_exists {
            if let Some(newpath) = map_library_path(abs_path) {
                debug!("new path is {}", newpath);
                abs_path = newpath;
            }
        }
    }

    // O_PATH creates a path-only descriptor. Linux ignores creation, truncation,
    // access-mode, and atime-related flags in this mode.
    let path_only = flags.contains(OpenFlags::O_PATH);
    let create = !path_only && flags.contains(OpenFlags::O_CREATE);
    let create_exclusive = create && flags.contains(OpenFlags::O_EXCL);

    //判断是否是设备文件。必须在 create_exclusive 检查之后，否则
    //mkdir("/dev/null") 会错误地返回成功。
    if find_device(abs_path) {
        // 检查挂载的 MS_NODEV 标志，nodev 挂载上不允许打开设备文件。
        {
            let mnt_table = MNT_TABLE.lock();
            if let Some((_, _, _, mount_flags)) = mnt_table.mount_for_path(abs_path) {
                if mount_flags.contains(MountFlags::NODEV) {
                    return Err(SysErrNo::EACCES);
                }
            }
        }
        if create_exclusive {
            return Err(SysErrNo::EEXIST);
        }
        if flags.contains(OpenFlags::O_DIRECTORY) {
            return Err(SysErrNo::ENOTDIR);
        }
        let device = open_device_file(abs_path)?;
        return Ok(FileClass::Abs(device));
    }

    // A cache hit already represents an alias bound by `insert_inode_idx()`.
    // Avoid probing it twice: apart from the duplicated map lookup, the old
    // `has_inode() + find_inode_idx()` sequence repeatedly entered the inode
    // alias-maintenance path on every cached open.
    let preserve_final_symlink = flags.intersects(OpenFlags::O_NOFOLLOW | OpenFlags::O_UNLINK);
    let mut inode = if !preserve_final_symlink {
        FsIndex::find_inode_idx(abs_path)
    } else {
        None
    };
    if inode.is_none() {
        let found_res = find_from_cached_parent(abs_path, flags)
            .unwrap_or_else(|| superblock_root_inode().find(abs_path, flags, 0));
        match found_res {
            Ok(t) => {
                inode = Some(if preserve_final_symlink {
                    t
                } else {
                    FsIndex::insert_inode_idx(abs_path, t)
                });
            }
            // `Ext4Inode::find()` only follows a final symlink.  A Debian
            // path such as `/bin/bash` therefore reports ENOTDIR while
            // traversing `/bin -> /usr/bin`; retry through its resolved
            // parent before treating it as a genuine non-directory error.
            Err(SysErrNo::ENOTDIR) => {
                let resolved_path = resolve_create_path(abs_path)?;
                if resolved_path == abs_path {
                    return Err(SysErrNo::ENOTDIR);
                }
                let found_res = find_from_cached_parent(&resolved_path, flags)
                    .unwrap_or_else(|| superblock_root_inode().find(&resolved_path, flags, 0));
                let resolved_inode = found_res?;
                inode = Some(if preserve_final_symlink {
                    resolved_inode
                } else {
                    let resolved_inode = FsIndex::insert_inode_idx(&resolved_path, resolved_inode);
                    FsIndex::insert_inode_idx(abs_path, resolved_inode.clone());
                    resolved_inode
                });
            }
            Err(SysErrNo::ELOOP) => return Err(SysErrNo::ELOOP),
            Err(_) => {
                if let Ok(resolved_path) = resolve_create_path(abs_path) {
                    if resolved_path != abs_path {
                        let found_res = find_from_cached_parent(&resolved_path, flags)
                            .unwrap_or_else(|| {
                                superblock_root_inode().find(&resolved_path, flags, 0)
                            });
                        if let Ok(t) = found_res {
                            inode = Some(if preserve_final_symlink {
                                t
                            } else {
                                let t = FsIndex::insert_inode_idx(&resolved_path, t);
                                FsIndex::insert_inode_idx(abs_path, t.clone());
                                t
                            });
                        }
                    }
                } else {
                    // warn!(
                    //     "Unexpected error in root_inode().find({},{:?},0)",
                    //     abs_path,
                    //     flags,
                    // );
                }
            }
        }
    }
    if let Some(inode) = inode {
        // The inode type is immutable for the lifetime of a VFS inode.  Read
        // it once so O_DIRECTORY, directory-write, device, and regular-file
        // checks do not repeat the same filesystem metadata lookup.
        let inode_type = inode.types();
        if create_exclusive {
            return Err(SysErrNo::EEXIST);
        }
        if flags.contains(OpenFlags::O_DIRECTORY) && inode_type != InodeType::Dir {
            return Err(SysErrNo::ENOTDIR);
        }

        let (readable, writable) = flags.read_write();
        let directory_write_intent = writable || (!path_only && flags.contains(OpenFlags::O_TRUNC));
        if inode_type.is_dir() && (directory_write_intent || create) {
            return Err(SysErrNo::EISDIR);
        }

        if !path_only {
            check_noatime_permission(&inode, flags)?;
        }
        // 检查挂载的 MS_NODEV 标志：文件系统上的设备节点（mknod 创建）
        // 在 nodev 挂载上也不允许打开。但 O_UNLINK（删除操作）应豁免，
        // 否则 cleanup 无法删除设备节点。
        if !flags.contains(OpenFlags::O_UNLINK) {
            if inode_type == InodeType::CharDevice || inode_type == InodeType::BlockDevice {
                let mnt_table = MNT_TABLE.lock();
                if let Some((_, _, _, mount_flags)) = mnt_table.mount_for_path(abs_path) {
                    if mount_flags.contains(MountFlags::NODEV) {
                        return Err(SysErrNo::EACCES);
                    }
                }
            }
        }
        // 如果以写模式打开，检查文件的写权限
        if writable {
            if let Some(task) = current_task() {
                let task_inner = task.inner_lock();
                debug!(
                    "[open] existing file writable check: uid={} euid={} abs_path={}",
                    task_inner.user_id, task_inner.effective_uid, abs_path
                );
                if task_inner.effective_uid != 0 {
                    let file_stat = inode.fstat();
                    let file_fmode = inode.fmode()?;
                    let file_mode = file_fmode & 0xfff;
                    let file_mode = FileMode::from_bits_truncate(file_mode);
                    let my_uid = task_inner.effective_uid;
                    let my_gid = task_inner.effective_gid;
                    let owner_uid = file_stat.st_uid;
                    let owner_gid = file_stat.st_gid;

                    debug!(
                        "[open] file mode={:o} owner_uid={} owner_gid={} my_uid={} my_gid={}",
                        file_fmode & 0xfff,
                        owner_uid,
                        owner_gid,
                        my_uid,
                        my_gid
                    );

                    let has_write = if my_uid == owner_uid {
                        file_mode.contains(FileMode::S_IWUSR)
                    } else if my_gid == owner_gid {
                        file_mode.contains(FileMode::S_IWGRP)
                    } else {
                        file_mode.contains(FileMode::S_IWOTH)
                    };
                    if !has_write {
                        debug!("[open] EACCES: no write permission on existing file");
                        return Err(SysErrNo::EACCES);
                    }
                }
            }
        }
        if inode_type.is_file() {
            let requester_pid = current_task()
                .map(|task| task.pid() as i32)
                .unwrap_or_default();
            file_lock::notify_file_lease_break(&inode.path(), requester_pid, writable);
        }
        let osfile = OSFile::new(
            readable,
            writable,
            flags.contains(OpenFlags::O_APPEND),
            inode,
        );
        if !path_only && flags.contains(OpenFlags::O_TRUNC) {
            osfile.inode.truncate(0)?;
        }
        return Ok(FileClass::File(Arc::new(osfile)));
    }

    // 节点不存在
    if create {
        debug!(
            "[open] file not found, calling create_file for {}",
            abs_path
        );
        return create_file(abs_path, flags, mode);
    }
    Err(SysErrNo::ENOENT)
}

/// 按常规用户态语义打开 `abs_path`。
///
/// 此入口会应用动态库路径映射；`mode` 仅在 `O_CREATE` 创建新节点时用于
/// 计算初始权限，实际权限还会受当前进程 umask 影响。
pub fn open(abs_path: &str, flags: OpenFlags, mode: u32) -> SysResult<FileClass> {
    open_inner(abs_path, flags, mode, true)
}

/// 打开 `abs_path`，但不应用动态库路径映射。
///
/// 供内核内部需要精确访问调用方路径的场景使用，其余打开语义与 [`open`] 相同。
pub fn open_direct(abs_path: &str, flags: OpenFlags, mode: u32) -> SysResult<FileClass> {
    open_inner(abs_path, flags, mode, false)
}
