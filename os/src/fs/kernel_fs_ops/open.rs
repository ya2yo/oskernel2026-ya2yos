use crate::fs::map_library_path;
use crate::task::current_task;
use crate::syscall::options::FaccessatFileMode;

use super::*;
use alloc::sync::Arc;
use log::{debug, warn};
fn create_file(abs_path: &str, flags: OpenFlags, mode: u32) -> Result<FileClass, SysErrNo> {
    // 检查父目录的写入和执行权限
    // 参考 faccessat 的权限检查逻辑
    if let Some(parent_path) = {
        let abs_path = abs_path.trim_end_matches('/');
        if abs_path.is_empty() || abs_path == "/" {
            None // 根目录没有父目录
        } else {
            let idx = abs_path.rfind('/').unwrap_or(0);
            if idx == 0 {
                Some("/")
            } else {
                Some(&abs_path[..idx])
            }
        }
    } {
        // 查找父目录的 inode
        let parent_inode_opt = if FsIndex::has_inode(parent_path) {
            FsIndex::find_inode_idx(parent_path)
        } else {
            match superblock_root_inode().find(parent_path, OpenFlags::empty(), 0) {
                Ok(inode) => {
                    FsIndex::insert_inode_idx(parent_path, inode.clone());
                    Some(inode)
                }
                Err(_) => None,
            }
        };

        if let Some(parent_inode) = parent_inode_opt {
            let parent_mode = parent_inode.fmode()? & 0xfff;
            let parent_mode = FaccessatFileMode::from_bits_truncate(parent_mode);

            if let Some(task) = current_task() {
                let task_inner = task.inner_lock();
                // root (uid 0) 绕过权限检查
                if task_inner.user_id != 0 {
                    // 检查父目录的可执行(搜索)权限
                    if !(parent_mode.contains(FaccessatFileMode::S_IXUSR)
                        || parent_mode.contains(FaccessatFileMode::S_IXGRP)
                        || parent_mode.contains(FaccessatFileMode::S_IXOTH))
                    {
                        return Err(SysErrNo::EACCES);
                    }
                    // 检查父目录的可写权限
                    if !(parent_mode.contains(FaccessatFileMode::S_IWUSR)
                        || parent_mode.contains(FaccessatFileMode::S_IWGRP)
                        || parent_mode.contains(FaccessatFileMode::S_IWOTH))
                    {
                        return Err(SysErrNo::EACCES);
                    }
                }
            }
        }
    }

    // 一定能找到,因为除了RootInode外都有父结点
    let parent_dir = superblock_root_inode();
    let (readable, writable) = flags.read_write();
    let inode = parent_dir.create(abs_path, flags.node_type())?;
    inode.fmode_set(mode);
    FsIndex::insert_inode_idx(abs_path, inode.clone());
    let osinode = OSFile::new(readable, writable, inode);
    Ok(FileClass::File(Arc::new(osinode)))
}
pub fn open(abs_path: &str, flags: OpenFlags, mode: u32) -> Result<FileClass, SysErrNo> {
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
    if let Some(newpath) = map_library_path(abs_path) {
        debug!("new path is {}", newpath);
        abs_path = newpath;
    }

    let mut inode: Option<Arc<dyn Inode>> = None;
    // 同一个路径对应一个Inode
    if FsIndex::has_inode(abs_path) {
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
            FsIndex::insert_inode_idx(abs_path, t.clone());
            inode = Some(t);
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
        if flags.contains(OpenFlags::O_DIRECTORY) && inode.types() != InodeType::Dir {
            return Err(SysErrNo::ENOTDIR);
        }
        let (readable, writable) = flags.read_write();
        let osfile = OSFile::new(readable, writable, inode);
        if flags.contains(OpenFlags::O_APPEND) {
            osfile.lseek(0, SEEK_END)?;
        }
        if flags.contains(OpenFlags::O_TRUNC) {
            osfile.inode.truncate(0)?;
        }
        return Ok(FileClass::File(Arc::new(osfile)));
    }

    // 节点不存在
    if flags.contains(OpenFlags::O_CREATE) {
        return create_file(abs_path, flags, mode);
    }
    Err(SysErrNo::ENOENT)
}
