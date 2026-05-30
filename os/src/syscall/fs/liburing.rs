use log::warn;

use crate::{syscall::fs::dummyfd_create, utils::SyscallRet};

/// https://man7.org/linux/man-pages/man2/io_uring_setup.2.html
pub fn sys_io_uring_setup(_entriers: u32, _params: *mut u8)->SyscallRet {
    warn!("[sys_io_uring_setup] not implement!");
    dummyfd_create()
}