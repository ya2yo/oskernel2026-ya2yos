use log::warn;

use crate::{syscall::fs::dummyfd_create, utils::SyscallRet};

/// https://man7.org/linux/man-pages/man2/userfaultfd.2.html
pub fn sys_user_faultfd(_flags: u32) -> SyscallRet {
    warn!("[sys_user_faultfd] not implement!");
    dummyfd_create()
}
