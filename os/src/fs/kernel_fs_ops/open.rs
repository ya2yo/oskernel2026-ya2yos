use crate::fs::{map_library_path, DentryLookup, DENTRY_CACHE};
use crate::syscall::{fs::file_lock, FaccessatFileMode};
use crate::task::current_task;
use crate::utils::SysResult;

use super::*;
use alloc::sync::Arc;
use alloc::{format, string::String};
use linux_raw_sys::general::CAP_FOWNER;
use log::{debug, warn};

fn split_parent_child(abs_path: &str) -> Option<(&str, &str)> {
    let abs_path = abs_path.trim_end_matches('/');
    if abs_path.is_empty() || abs_path == "/" {
        return None;
    }

    let idx = abs_path.rfind('/').unwrap_or(0);
    let parent = if idx == 0 { "/" } else { &abs_path[..idx] };
    Some((parent, &abs_path[idx + 1..]))
}

fn join_parent_child(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{}", child)
    } else {
        format!("{}/{}", parent, child)
    }
}

struct ParentPath {
    parent_inode: Arc<dyn Inode>,
    create_path: String,
    child_name: String,
}

fn resolve_parent_path(abs_path: &str) -> SysResult<ParentPath> {
    let Some((parent_path, child_name)) = split_parent_child(abs_path) else {
        return Err(SysErrNo::ENOENT);
    };

    let parent_inode = if FsIndex::has_inode(parent_path) {
        FsIndex::find_inode_idx(parent_path).ok_or(SysErrNo::ENOENT)?
    } else {
        let inode = superblock_root_inode().find(parent_path, OpenFlags::O_DIRECTORY, 0)?;
        FsIndex::insert_inode_idx(parent_path, inode)
    };

    if !parent_inode.types().is_dir() {
        return Err(SysErrNo::ENOTDIR);
    }

    Ok(ParentPath {
        create_path: join_parent_child(&parent_inode.path(), child_name),
        parent_inode,
        child_name: String::from(child_name),
    })
}

fn resolve_create_path(abs_path: &str) -> SysResult<String> {
    resolve_parent_path(abs_path).map(|target| target.create_path)
}

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

fn find_from_cached_parent(abs_path: &str, flags: OpenFlags) -> Option<SysResult<Arc<dyn Inode>>> {
    let (parent_path, child_name) = split_parent_child(abs_path)?;
    let parent_inode = FsIndex::find_inode_idx(parent_path)?;
    if !parent_inode.types().is_dir() {
        return Some(Err(SysErrNo::ENOTDIR));
    }

    if !flags.contains(OpenFlags::O_NOFOLLOW) {
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
        let inode = FsIndex::insert_inode_idx(&lookup_path, inode);
        if lookup_path != abs_path {
            FsIndex::insert_inode_idx(abs_path, inode.clone());
        }
        if !flags.contains(OpenFlags::O_NOFOLLOW) {
            DENTRY_CACHE.insert_positive(&parent_inode, child_name, inode.clone());
        }
        inode
    });
    if found.as_ref().err() == Some(&SysErrNo::ENOENT)
        && !flags.contains(OpenFlags::O_NOFOLLOW)
        && !flags.contains(OpenFlags::O_CREATE)
    {
        DENTRY_CACHE.insert_negative(&parent_inode, child_name);
    }
    Some(found)
}

fn cache_created_dentry(parent_inode: &Arc<dyn Inode>, child_name: &str, inode: Arc<dyn Inode>) {
    if !child_name.is_empty() {
        DENTRY_CACHE.insert_positive(parent_inode, child_name, inode);
    }
}

fn invalidate_dentry(parent_inode: &Arc<dyn Inode>, child_name: &str) {
    if !child_name.is_empty() {
        DENTRY_CACHE.invalidate(parent_inode, child_name);
    }
}

pub fn invalidate_dentry_path(abs_path: &str) {
    if let Some((parent_path, child_name)) = split_parent_child(abs_path) {
        if let Some(parent_inode) = FsIndex::find_inode_idx(parent_path) {
            invalidate_dentry(&parent_inode, child_name);
        }
    }
}

pub fn cache_positive_dentry_path(abs_path: &str, inode: Arc<dyn Inode>) {
    if let Some((parent_path, child_name)) = split_parent_child(abs_path) {
        if let Some(parent_inode) = FsIndex::find_inode_idx(parent_path) {
            cache_created_dentry(&parent_inode, child_name, inode);
        }
    }
}

fn create_file(abs_path: &str, flags: OpenFlags, mode: u32) -> SysResult<FileClass> {
    debug!(
        "[create_file] abs_path={}, flags={:?}, mode={:o}",
        abs_path, flags, mode
    );
    let target = resolve_parent_path(abs_path)?;
    let create_path = target.create_path;
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
            let parent_mode = FaccessatFileMode::from_bits_truncate((pstat.st_mode & 0xfff) as u32);
            let owner_uid = pstat.st_uid;
            let owner_gid = pstat.st_gid;

            // 确定进程属于 owner / group / other 哪一类。
            let (has_write, has_exec) = if my_uid == owner_uid {
                (
                    parent_mode.contains(FaccessatFileMode::S_IWUSR),
                    parent_mode.contains(FaccessatFileMode::S_IXUSR),
                )
            } else if my_gid == owner_gid {
                (
                    parent_mode.contains(FaccessatFileMode::S_IWGRP),
                    parent_mode.contains(FaccessatFileMode::S_IXGRP),
                )
            } else {
                (
                    parent_mode.contains(FaccessatFileMode::S_IWOTH),
                    parent_mode.contains(FaccessatFileMode::S_IXOTH),
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
fn open_inner(
    abs_path: &str,
    flags: OpenFlags,
    mode: u32,
    map_dynamic: bool,
) -> SysResult<FileClass> {
    debug!("open({},{:?},{})", abs_path, flags, mode);
    // log::info!("[open] abs_path={}", abs_path);
    //判断是否是设备文件
    if find_device(abs_path) {
        let device = open_device_file(abs_path)?;
        return Ok(FileClass::Abs(device));
    }
    // 如果是动态链接文件,转换路径
    // HXC: 转个锤子，本来输入的路径就是转换后的
    // HXC: 我错了还是得转，因为用户态也可能想要这个文件
    // 也许以后可以改成符号链接实现这种映射？
    debug!("abs_path is {}", abs_path);
    let mut abs_path: &str = abs_path;
    if map_dynamic {
        if let Some(newpath) = map_library_path(abs_path) {
            debug!("new path is {}", newpath);
            abs_path = newpath;
        }
    }

    if flags.contains(OpenFlags::O_CREATE) && flags.contains(OpenFlags::O_EXCL) {
        return create_file(abs_path, flags, mode);
    }

    let mut inode: Option<Arc<dyn Inode>> = None;
    // 同一个路径对应一个Inode
    if !flags.contains(OpenFlags::O_NOFOLLOW) && FsIndex::has_inode(abs_path) {
        inode = FsIndex::find_inode_idx(abs_path);
    } else {
        let found_res = find_from_cached_parent(abs_path, flags)
            .unwrap_or_else(|| superblock_root_inode().find(abs_path, flags, 0));
        match found_res {
            Ok(t) => {
                inode = Some(FsIndex::insert_inode_idx(abs_path, t));
            }
            Err(SysErrNo::ENOTDIR) => return Err(SysErrNo::ENOTDIR),
            Err(SysErrNo::ELOOP) => return Err(SysErrNo::ELOOP),
            Err(_) => {
                if let Ok(resolved_path) = resolve_create_path(abs_path) {
                    if resolved_path != abs_path {
                        let found_res = find_from_cached_parent(&resolved_path, flags)
                            .unwrap_or_else(|| {
                                superblock_root_inode().find(&resolved_path, flags, 0)
                            });
                        if let Ok(t) = found_res {
                            let t = FsIndex::insert_inode_idx(&resolved_path, t);
                            FsIndex::insert_inode_idx(abs_path, t.clone());
                            inode = Some(t);
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
        if flags.contains(OpenFlags::O_CREATE) && flags.contains(OpenFlags::O_EXCL) {
            return Err(SysErrNo::EEXIST);
        }
        if flags.contains(OpenFlags::O_DIRECTORY) && inode.types() != InodeType::Dir {
            return Err(SysErrNo::ENOTDIR);
        }
        check_noatime_permission(&inode, flags)?;
        let (readable, writable) = flags.read_write();
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
                    let file_mode = FaccessatFileMode::from_bits_truncate(file_mode);
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
                        file_mode.contains(FaccessatFileMode::S_IWUSR)
                    } else if my_gid == owner_gid {
                        file_mode.contains(FaccessatFileMode::S_IWGRP)
                    } else {
                        file_mode.contains(FaccessatFileMode::S_IWOTH)
                    };
                    if !has_write {
                        debug!("[open] EACCES: no write permission on existing file");
                        return Err(SysErrNo::EACCES);
                    }
                }
            }
        }
        if inode.types().is_file() {
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
        if flags.contains(OpenFlags::O_TRUNC) {
            osfile.inode.truncate(0)?;
        }
        return Ok(FileClass::File(Arc::new(osfile)));
    }

    // 节点不存在
    if flags.contains(OpenFlags::O_CREATE) {
        debug!(
            "[open] file not found, calling create_file for {}",
            abs_path
        );
        return create_file(abs_path, flags, mode);
    }
    Err(SysErrNo::ENOENT)
}

pub fn open(abs_path: &str, flags: OpenFlags, mode: u32) -> SysResult<FileClass> {
    open_inner(abs_path, flags, mode, true)
}

pub fn open_direct(abs_path: &str, flags: OpenFlags, mode: u32) -> SysResult<FileClass> {
    open_inner(abs_path, flags, mode, false)
}
