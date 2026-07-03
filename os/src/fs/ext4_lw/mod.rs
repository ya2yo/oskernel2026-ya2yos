//! lwext4-backed EXT4 文件系统适配层。
//!
//! 该模块把第三方 `lwext4_rust` wrapper 暴露的路径式文件系统接口适配成
//! Ya2yOS 内部的 VFS trait：
//!
//! - [`inode`]：将 `Ext4File` 包装成 VFS [`Inode`](crate::fs::Inode)。
//! - [`sb`]：维护全局超级块、挂载点状态和块设备读写回调。
//!
//! 上层 syscall/VFS 只依赖 `superblock_*` helper 和 `Inode` trait，不直接接触
//! lwext4 的 C 风格接口。

mod inode;
mod sb;

pub use inode::*;
pub use sb::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
