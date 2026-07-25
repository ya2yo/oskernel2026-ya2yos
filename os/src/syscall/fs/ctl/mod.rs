//! 文件系统控制类 syscall 门面。
//!
//! 原先聚集于本文件的目录项、链接、时间、元数据和 ioctl 路径已经按职责拆分至
//! `ctl/`。这里保留共享依赖和公开 re-export，保证 `fs::ctl::*` 与 syscall
//! 分发的调用接口不变。

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use linux_raw_sys::general::{AT_EMPTY_PATH, AT_REMOVEDIR, AT_SYMLINK_FOLLOW, AT_SYMLINK_NOFOLLOW};
use linux_raw_sys::loop_device::LOOP_SET_FD;
use log::debug;

use super::path::{mode_allows, parse_proc_self_fd};
use crate::fs::{
    cache_positive_dentry_path, invalidate_dentry_path, open, superblock_root_inode,
    superblock_sync, File, FsIndex, Inode, InodeType, MountFlags, OpenFlags, MAX_PATH_LEN,
    MNT_TABLE, NONE_MODE, SEEK_CUR, SEEK_SET,
};
use crate::mm::{
    copy_from_user, copy_to_user, if_bad_address, read_user_cstr, user_buffer_from_kernel,
};
use crate::syscall::options::FileMode;
use crate::task::{current_task, Process};
use crate::timer::{get_time_ms, Timespec, NOW_TIME_STAMP};
use crate::utils::{
    get_abs_path as normalize_abs_path, is_abs_path, rsplit_once, SysErrNo, SyscallRet,
};

mod common;
mod directory;
mod ioctl;
mod link;
mod metadata;
mod namespace;
mod time;
mod unlink;

use common::{
    check_hard_link_limit, check_link_mounts, check_path_argument,
    has_self_referential_symlink_prefix, parent_path_of, resolve_linkat_path, LINKAT_VALID_FLAGS,
};
use namespace::check_parent_permission;

pub use common::has_too_long_path_component;
pub use directory::*;
pub use ioctl::*;
pub use link::*;
pub use metadata::*;
pub use namespace::*;
pub use time::*;
pub use unlink::*;
