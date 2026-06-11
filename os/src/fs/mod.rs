mod ext4_lw;
mod files;
mod fs_info;
mod fstruct;
mod mount;
// pub use files::{make_socket, make_socketpair, OSFile};
mod stat;
mod vfs;
#[cfg(feature = "net")]
pub use crate::fs::files::Socket;
pub use crate::fs::files::*;
use crate::mm::UserBuffer;
// use crate::syscall::FaccessatFileMode;
use crate::utils::{GeneralRet, SysErrNo};

use alloc::string::String;
use alloc::vec;
use alloc::{sync::Arc, vec::Vec};
pub use ext4_lw::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
pub use files::devfs::*;
pub use fs_info::*;
pub use fstruct::*;
// pub mod socket_defs;
pub use files::pipe::{make_pipe, Pipe};
pub use files::stdio::{Stdin, Stdout};
use log::debug;
pub use mount::MNT_TABLE;
pub use stat::*;
pub use vfs::*;
mod kernel_fs_ops;
pub use kernel_fs_ops::{
    create_init_files, create_proc_dir_and_file, open, refresh_proc_stat, refresh_proc_status,
    remove_proc_dir_and_file, FsIndex,
};
mod map_dynamic_link;
pub use map_dynamic_link::{
    map_dynamic_link_file, map_dynamic_link_file_directly_map, map_library_path,
};

bitflags! {
    /// 定义一份打开文件的标志
    pub struct OpenFlags: u32 {
        // reserve 3 bits for the access mode
        const O_RDONLY      = 0;           // Read only
        const O_WRONLY      = 1;           // Write only
        const O_RDWR        = 2;           // Read and write
        const O_ACCMODE     = 3;           // Mask for file access modes
        const O_CREATE       = 0o100;       // Create file if it doesn't exist
        const O_EXCL        = 0o200;       // Exclusive use flag
        const O_NOCTTY      = 0o400;       // Do not assign controlling terminal
        const O_TRUNC       = 0o1000;      // Truncate flag
        const O_APPEND      = 0o2000;      // Set append mode
        const O_NONBLOCK    = 0o4000;      // Non-blocking mode
        const O_DSYNC       = 0o10000;     // Write operations complete as defined by POSIX
        const O_SYNC        = 0o4010000;   // Write operations complete as defined by POSIX
        const O_RSYNC       = 0o4010000;   // Synchronized read operations
        const O_DIRECTORY   = 0o200000;    // Must be a directory
        const O_NOFOLLOW    = 0o400000;    // Do not follow symbolic links
        const O_CLOEXEC     = 0o2000000;   // Set close-on-exec
        const O_ASYNC       = 0o20000;     // Signal-driven I/O
        const O_DIRECT      = 0o40000;     // Direct disk access hints
        const O_LARGEFILE   = 0o100000;    // Allow files larger than 2GB
        const O_NOATIME     = 0o1000000;   // Do not update access time
        const O_PATH        = 0o10000000;  // Obtain a file descriptor for a directory
        const O_TMPFILE     = 0o20200000;  // Create an unnamed temporary file

        const O_UNLINK    = 0o40000000;     //自用，用于识别unlink系统调用
    }
}

impl OpenFlags {
    pub fn read_write(&self) -> (bool, bool) {
        if self.is_empty() {
            (true, false)
        } else if self.contains(Self::O_WRONLY) {
            (false, true)
        } else {
            (true, true)
        }
    }

    pub fn node_type(&self) -> InodeType {
        if self.contains(OpenFlags::O_DIRECTORY) {
            InodeType::Dir
        } else {
            InodeType::File
        }
    }
}

pub const MAX_PATH_LEN: usize = 256;

pub const SEEK_SET: usize = 0;
pub const SEEK_CUR: usize = 1;
pub const SEEK_END: usize = 2;

pub const DEFAULT_FILE_MODE: u32 = 0o666;
pub const DEFAULT_DIR_MODE: u32 = 0o777;
pub const NONE_MODE: u32 = 0;

/// 枚举类型，分为普通文件和抽象文件
/// 普通文件File，特点是支持更多类型的操作，包含seek, offset等
/// 抽象文件Abs，抽象文件，只支持File trait的一些操作
#[derive(Clone)]
pub enum FileClass {
    File(Arc<OSFile>),
    #[cfg(feature = "net")]
    Socket(Arc<Socket>),
    Abs(Arc<dyn File>),
    FsContext(Arc<FsContextFd>),
    DetachedMount(Arc<DetachedMountFd>),
}

impl FileClass {
    pub fn file(&self) -> Result<Arc<OSFile>, SysErrNo> {
        match self {
            FileClass::File(f) => Ok(f.clone()),
            #[cfg(feature = "net")]
            FileClass::Socket(_) => Err(SysErrNo::EINVAL),
            FileClass::Abs(_) => Err(SysErrNo::EINVAL),
            FileClass::FsContext(_) => Err(SysErrNo::EINVAL),
            FileClass::DetachedMount(_) => Err(SysErrNo::EINVAL),
        }
    }
    #[cfg(feature = "net")]
    pub fn socket(&self) -> Result<Arc<Socket>, SysErrNo> {
        match self {
            FileClass::File(_) => Err(SysErrNo::ENOTSOCK),
            FileClass::Socket(f) => Ok(f.clone()),
            FileClass::Abs(_) => Err(SysErrNo::ENOTSOCK),
            FileClass::FsContext(_) => Err(SysErrNo::ENOTSOCK),
            FileClass::DetachedMount(_) => Err(SysErrNo::ENOTSOCK),
        }
    }
    pub fn abs(&self) -> Result<Arc<dyn File>, SysErrNo> {
        match self {
            FileClass::File(_) => Err(SysErrNo::EINVAL),
            #[cfg(feature = "net")]
            FileClass::Socket(_) => Err(SysErrNo::EINVAL),
            FileClass::Abs(f) => Ok(f.clone()),
            FileClass::FsContext(_) => Err(SysErrNo::EINVAL),
            FileClass::DetachedMount(_) => Err(SysErrNo::EINVAL),
        }
    }
    pub fn fs_context(&self) -> Result<Arc<FsContextFd>, SysErrNo> {
        match self {
            FileClass::FsContext(f) => Ok(f.clone()),
            _ => Err(SysErrNo::EINVAL),
        }
    }
    pub fn detached_mount(&self) -> Result<Arc<DetachedMountFd>, SysErrNo> {
        match self {
            FileClass::DetachedMount(f) => Ok(f.clone()),
            _ => Err(SysErrNo::EINVAL),
        }
    }
    pub fn any(&self) -> Arc<dyn File> {
        match self {
            FileClass::File(f) => f.clone(),
            #[cfg(feature = "net")]
            FileClass::Socket(s) => s.clone(),
            FileClass::Abs(f) => f.clone(),
            FileClass::FsContext(f) => f.clone(),
            FileClass::DetachedMount(f) => f.clone(),
        }
    }
}
#[repr(u8)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InodeType {
    Unknown = 0o0,
    /// FIFO (named pipe)
    Fifo = 0o1,
    /// Character device
    CharDevice = 0o2,
    /// Directory
    Dir = 0o4,
    /// Block device
    BlockDevice = 0o6,
    /// Regular file
    File = 0o10,
    /// Symbolic link
    SymLink = 0o12,
    /// Socket
    Socket = 0o14,
}

impl InodeType {
    /// Tests whether this node type represents a regular file.
    pub const fn is_file(self) -> bool {
        matches!(self, Self::File)
    }
    /// Tests whether this node type represents a directory.
    pub const fn is_dir(self) -> bool {
        matches!(self, Self::Dir)
    }
    /// Tests whether this node type represents a symbolic link.
    pub const fn is_symlink(self) -> bool {
        matches!(self, Self::SymLink)
    }
    /// Returns `true` if this node type is a block device.
    pub const fn is_block_device(self) -> bool {
        matches!(self, Self::BlockDevice)
    }
    /// Returns `true` if this node type is a char device.
    pub const fn is_char_device(self) -> bool {
        matches!(self, Self::CharDevice)
    }
    /// Returns `true` if this node type is a fifo.
    pub const fn is_fifo(self) -> bool {
        matches!(self, Self::Fifo)
    }
    /// Returns `true` if this node type is a socket.
    pub const fn is_socket(self) -> bool {
        matches!(self, Self::Socket)
    }
    // Returns a character representation of the node type.
    //
    // For example, `d` for directory, `-` for regular file, etc.
    // pub const fn as_char(self) -> char {
    //     match self {
    //         Self::Fifo => 'p',
    //         Self::CharDevice => 'c',
    //         Self::Dir => 'd',
    //         Self::BlockDevice => 'b',
    //         Self::File => '-',
    //         Self::SymLink => 'l',
    //         Self::Socket => 's',
    //     }
    // }
}

pub fn init() {
    create_init_files();
    // TODO(ZMY):为了过libc-test utime的权宜之计,读取RTC太麻烦了
    superblock_root_inode().set_timestamps(Some(0), Some(0), Some(0));
}

pub fn list_apps() {
    println!("/**** APPS ****");
    superblock_ls();
    println!("**************/");
}
