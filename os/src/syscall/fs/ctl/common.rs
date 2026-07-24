//! 文件系统控制类 syscall。
//!
//! 本模块承载会创建、删除、重命名目录项，或修改 inode 元数据/文件描述符
//! 控制状态的接口，例如 `ioctl`、`mknodat`、`mkdirat`、`linkat`、
//! `unlinkat`、`symlinkat`、`renameat2`、`utimensat`、`chown/chmod`
//! 和 `sync`。纯路径上下文接口放在 `path.rs`，状态查询接口放在
//! `stat.rs`，避免路径解析、元数据查询和控制操作混在同一个文件中。

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use linux_raw_sys::general::{AT_EMPTY_PATH, AT_REMOVEDIR, AT_SYMLINK_FOLLOW, AT_SYMLINK_NOFOLLOW};
use log::debug;

use super::super::path::{mode_allows, parse_proc_self_fd};
use crate::fs::{
    cache_positive_dentry_path, invalidate_dentry_path, open, superblock_root_inode,
    superblock_sync, File, FsIndex, Inode, InodeType, MountFlags, OpenFlags, MAX_PATH_LEN, MNT_TABLE,
    NONE_MODE, SEEK_CUR, SEEK_SET,
};
use crate::mm::{
    copy_from_user, copy_to_user, if_bad_address, read_user_cstr, user_buffer_from_kernel,
};
use crate::syscall::options::FaccessatFileMode;
use crate::task::{current_task, Process};
use crate::timer::{get_time_ms, Timespec, NOW_TIME_STAMP};
use crate::utils::{
    get_abs_path as normalize_abs_path, is_abs_path, rsplit_once, SysErrNo, SyscallRet,
};
use linux_raw_sys::loop_device::LOOP_SET_FD;

const MAX_FILE_NAME_LEN: usize = 255;
// Keep the exposed hard-link limit finite so LTP hard-link limit probes can
// terminate quickly. POSIX only requires LINK_MAX to be at least 8.
const MAX_HARD_LINKS: u32 = 1024;
pub(super) const LINKAT_VALID_FLAGS: u32 = (AT_EMPTY_PATH | AT_SYMLINK_FOLLOW) as u32;

pub fn has_too_long_path_component(path: &str) -> bool {
    path.split('/')
        .any(|component| component.len() > MAX_FILE_NAME_LEN)
}

/// 路径类 syscall 需要在路径解析前处理空路径和超长路径，否则空相对路径会被
/// `get_abs_path()` 解释成 cwd，超长路径也会落到底层查找并错误返回 `ENOENT`。
pub(super) fn check_path_argument(path: &str, allow_empty: bool, check_length: bool) -> SyscallRet {
    if path.is_empty() {
        return if allow_empty {
            Ok(0)
        } else {
            Err(SysErrNo::ENOENT)
        };
    }
    if check_length && (path.len() >= MAX_PATH_LEN || has_too_long_path_component(path)) {
        return Err(SysErrNo::ENAMETOOLONG);
    }
    Ok(0)
}

/// 返回目标路径的父目录；hard link 创建权限检查发生在父目录上。
pub(super) fn parent_path_of(abs_path: &str) -> Result<&str, SysErrNo> {
    if abs_path.is_empty() || abs_path == "/" {
        return Err(SysErrNo::ENOENT);
    }
    Ok(rsplit_once(abs_path.trim_end_matches('/'), "/").0)
}

/// hard link 不能跨挂载点创建；在只读挂载点内创建新目录项也应返回 `EROFS`。
pub(super) fn check_link_mounts(old_abs_path: &str, new_abs_path: &str) -> SyscallRet {
    // Ya2yOS currently exposes proc compatibility files through the root ext4
    // backend. They still form a distinct Linux-visible pseudo-filesystem, so
    // hard links between /proc and an ordinary path must fail with EXDEV.
    let old_is_procfs = old_abs_path == "/proc" || old_abs_path.starts_with("/proc/");
    let new_is_procfs = new_abs_path == "/proc" || new_abs_path.starts_with("/proc/");
    if old_is_procfs != new_is_procfs {
        return Err(SysErrNo::EXDEV);
    }

    let mnt_table = MNT_TABLE.lock();
    let old_mount = mnt_table.mount_for_path(old_abs_path);
    let new_mount = mnt_table.mount_for_path(new_abs_path);
    drop(mnt_table);

    let old_mount_dir = old_mount.as_ref().map(|(_, dir, _, _)| dir);
    let new_mount_dir = new_mount.as_ref().map(|(_, dir, _, _)| dir);
    if old_mount_dir != new_mount_dir {
        return Err(SysErrNo::EXDEV);
    }

    if let Some((_, _, _, mountflags)) = new_mount.or(old_mount) {
        if mountflags.contains(MountFlags::RDONLY) {
            return Err(SysErrNo::EROFS);
        }
    }
    Ok(0)
}

/// Resolve a linkat pathname while enforcing the Linux dirfd contract for a
/// relative path. `Process::get_abs_path()` is intentionally generic and
/// accepts any open file descriptor as a base; linkat requires that base to be
/// a directory and reports ENOTDIR for stdin, pipes, sockets, and regular files.
pub(super) fn resolve_linkat_path(
    proc: &Process,
    dirfd: isize,
    path: &str,
) -> Result<String, SysErrNo> {
    if is_abs_path(path) || dirfd == -100 {
        return proc.get_abs_path(dirfd, path);
    }
    if dirfd < 0 {
        return Err(SysErrNo::EBADF);
    }

    let dir = proc
        .fd_table
        .get(dirfd as usize)?
        .file()
        .map_err(|_| SysErrNo::ENOTDIR)?;
    if !dir.inode.types().is_dir() {
        return Err(SysErrNo::ENOTDIR);
    }
    Ok(normalize_abs_path(&dir.inode.path(), path))
}

/// 判断 `path` 是否位于 `ancestor` 子树内，用于识别 symlink 目标回指祖先目录。
fn path_is_same_or_ancestor(ancestor: &str, path: &str) -> bool {
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .map_or(false, |rest| rest.starts_with('/'))
}

/// 旧路径达到读取上限时，先扫描路径前缀里的 symlink，避免自引用环被误报为
/// `ENAMETOOLONG`。这里只处理 `link08` 覆盖的相对目标回指祖先目录场景。
pub(super) fn has_self_referential_symlink_prefix(abs_path: &str) -> bool {
    let mut prefix = String::new();
    for component in abs_path
        .split('/')
        .filter(|component| !component.is_empty())
    {
        prefix.push('/');
        prefix.push_str(component);

        let Ok(file) = open(
            &prefix,
            OpenFlags::O_RDONLY | OpenFlags::O_UNLINK,
            NONE_MODE,
        ) else {
            continue;
        };
        let Ok(file) = file.file() else {
            continue;
        };
        if file.inode.types() != InodeType::SymLink {
            continue;
        }

        let mut link_buf = [0u8; MAX_PATH_LEN];
        let Ok(len) = file.inode.read_link(&mut link_buf, MAX_PATH_LEN) else {
            continue;
        };
        let Ok(target) = core::str::from_utf8(&link_buf[..len]) else {
            continue;
        };
        let Ok(parent_path) = parent_path_of(&prefix) else {
            continue;
        };
        let target_abs = normalize_abs_path(parent_path, target);
        if path_is_same_or_ancestor(&target_abs, &prefix) {
            return true;
        }
    }
    false
}

/// 在创建新 hard link 前检查源 inode 的链接数上限，达到上限时返回 `EMLINK`。
pub(super) fn check_hard_link_limit(inode: &Arc<dyn Inode>) -> SyscallRet {
    if inode.fstat().st_nlink >= MAX_HARD_LINKS {
        return Err(SysErrNo::EMLINK);
    }
    Ok(0)
}
