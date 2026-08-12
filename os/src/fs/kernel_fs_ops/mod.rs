// 该模块存放给内核用的fs相关函数
// 包括open, FsIndex系列, create_init_files等

mod fsidx;
mod initfiles;
mod open;
mod proc_file;
use super::{
    find_device, open_device_file, register_device, superblock_root_inode, File, FileClass, Inode,
    InodeType, OSFile, OpenFlags, UptimeFile, DEFAULT_DIR_MODE, DEFAULT_FILE_MODE, SEEK_END,
};
use crate::utils::SysErrNo;
pub use fsidx::FsIndex;
pub use initfiles::create_init_files;
pub use open::{cache_positive_dentry_path, invalidate_dentry_path, open, open_direct};
pub use proc_file::{
    create_proc_dir, create_proc_dir_and_file, ensure_proc_dir, ensure_proc_path,
    materialize_proc_dirs, refresh_proc_maps, refresh_proc_stat, refresh_proc_status,
    remove_proc_dir_and_file,
};
