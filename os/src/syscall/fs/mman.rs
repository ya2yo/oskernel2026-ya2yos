use log::warn;

use crate::{syscall::fs::dummyfd_create, utils::SyscallRet};

/// https://man7.org/linux/man-pages/man2/memfd_create.2.html
pub fn sys_memfd_create(_name: *const u8, _flags: u32)->SyscallRet {
    warn!("[sys_memfd_create] not implement!");
    dummyfd_create()
}

/// https://www.man7.org/linux/man-pages//man2/memfd_secret.2.html
pub fn sys_memfd_secret(_flags: u32) -> SyscallRet {
    warn!("[sys_memfd_secret] not implement!");
    dummyfd_create()
}