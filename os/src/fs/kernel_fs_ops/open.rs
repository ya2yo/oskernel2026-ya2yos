use crate::fs::map_library_path;
use crate::syscall::FaccessatFileMode;
use crate::task::current_task;
use crate::utils::SysResult;

use super::*;
use alloc::sync::Arc;
use alloc::{format, string::String};
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

fn resolve_create_path(abs_path: &str) -> SysResult<String> {
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

    Ok(join_parent_child(&parent_inode.path(), child_name))
}

fn create_file(abs_path: &str, flags: OpenFlags, mode: u32) -> SysResult<FileClass> {
    debug!(
        "[create_file] abs_path={}, flags={:?}, mode={:o}",
        abs_path, flags, mode
    );
    let create_path = resolve_create_path(abs_path)?;
    // 检查父目录的写入和执行权限
    // 参考 faccessat 的权限检查逻辑
    if let Some(parent_path) = split_parent_child(&create_path).map(|(parent_path, _)| parent_path)
    {
        // debug!("[create_file] parent_path={}", parent_path);
        // 查找父目录的 inode
        let parent_inode_opt = if FsIndex::has_inode(parent_path) {
            FsIndex::find_inode_idx(parent_path)
        } else {
            match superblock_root_inode().find(parent_path, OpenFlags::empty(), 0) {
                Ok(inode) => Some(FsIndex::insert_inode_idx(parent_path, inode)),
                Err(e) => {
                    debug!("[create_file] parent inode not found: {:?}", e);
                    None
                }
            }
        };

        if let Some(parent_inode) = parent_inode_opt {
            let parent_fmode = parent_inode.fmode()?;
            let parent_mode = parent_fmode & 0xfff;
            let parent_mode = FaccessatFileMode::from_bits_truncate(parent_mode);

            if let Some(task) = current_task() {
                let task_inner = task.inner_lock();
                // debug!(
                //     "[create_file] uid={} euid={} gid={} egid={} parent_mode={:o}",
                //     task_inner.user_id,
                //     task_inner.effective_uid,
                //     task_inner.real_gid,
                //     task_inner.effective_gid,
                //     parent_fmode & 0xfff
                // );
                // root (euid 0) 绕过权限检查
                // 使用 effective_uid，因为 Linux 文件权限检查基于 effective uid
                if task_inner.effective_uid != 0 {
                    // 获取父目录的 owner uid/gid，用于判断进程是 owner/group/other
                    let pstat = parent_inode.fstat();
                    let owner_uid = pstat.st_uid;
                    let owner_gid = pstat.st_gid;
                    let my_uid = task_inner.effective_uid;
                    let my_gid = task_inner.effective_gid;

                    // debug!(
                    //     "[create_file] owner_uid={} owner_gid={} my_uid={} my_gid={}",
                    //     owner_uid, owner_gid, my_uid, my_gid
                    // );

                    // 确定进程属于 owner / group / other 哪一类
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

                    // debug!(
                    //     "[create_file] has_write={} has_exec={}",
                    //     has_write, has_exec
                    // );
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
        } else {
            debug!("[create_file] parent inode not found, skip permission check");
        }
    }

    // 一定能找到,因为除了RootInode外都有父结点
    let parent_dir = superblock_root_inode();
    let (readable, writable) = flags.read_write();
    let inode = parent_dir.create(&create_path, flags.node_type())?;
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
    let effective_mode = mode & !umask;
    // debug!(
    //     "[create_file] mode={:o} umask={:o} → effective={:o}",
    //     mode, umask, effective_mode
    // );
    inode.fmode_set(effective_mode);
    if let Some(task) = current_task() {
        let task_inner = task.inner_lock();
        // Linux assigns new inode gid from the parent directory when S_ISGID is set.
        let (uid, gid) = if let Some((parent_path, _)) = split_parent_child(&create_path) {
            let parent_inode = FsIndex::find_inode_idx(parent_path).or_else(|| {
                superblock_root_inode()
                    .find(parent_path, OpenFlags::O_DIRECTORY, 0)
                    .ok()
            });
            if let Some(parent_inode) = parent_inode {
                let parent_stat = parent_inode.fstat();
                let parent_mode = parent_inode.fmode()? & 0o7777;
                let gid = if parent_mode & 0o2000 != 0 {
                    parent_stat.st_gid
                } else {
                    task_inner.effective_gid
                };
                (task_inner.effective_uid, gid)
            } else {
                (task_inner.effective_uid, task_inner.effective_gid)
            }
        } else {
            (task_inner.effective_uid, task_inner.effective_gid)
        };
        inode.owner_set(uid, gid)?;
    }
    let inode = FsIndex::insert_inode_idx(&create_path, inode);
    if create_path != abs_path {
        FsIndex::insert_inode_idx(abs_path, inode.clone());
    }
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
        let found_res = superblock_root_inode().find(abs_path, flags, 0);
        if found_res.clone().err() == Some(SysErrNo::ENOTDIR) {
            return Err(SysErrNo::ENOTDIR);
        }
        if found_res.clone().err() == Some(SysErrNo::ELOOP) {
            return Err(SysErrNo::ELOOP);
        }
        if let Ok(t) = found_res {
            inode = Some(FsIndex::insert_inode_idx(abs_path, t));
        } else if let Ok(resolved_path) = resolve_create_path(abs_path) {
            if resolved_path != abs_path {
                let found_res = superblock_root_inode().find(&resolved_path, flags, 0);
                if let Ok(t) = found_res {
                    let t = FsIndex::insert_inode_idx(&resolved_path, t);
                    FsIndex::insert_inode_idx(abs_path, t.clone());
                    inode = Some(t);
                }
            }
        } else {
            // warn!(
            //     "Unexpected error in root_inode().find({},{:?},0):{:?}",
            //     abs_path,
            //     flags,
            //     found_res.clone().err().unwrap()
            // );
        }
    }
    if let Some(inode) = inode {
        if flags.contains(OpenFlags::O_CREATE) && flags.contains(OpenFlags::O_EXCL) {
            return Err(SysErrNo::EEXIST);
        }
        if flags.contains(OpenFlags::O_DIRECTORY) && inode.types() != InodeType::Dir {
            return Err(SysErrNo::ENOTDIR);
        }
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
