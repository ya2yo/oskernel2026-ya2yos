use log::debug;

use crate::utils::{SysErrNo, SyscallRet};

/// 参考 https://man7.org/linux/man-pages/man2/unshare.2.html
///
/// 允许进程（或线程）解除其执行上下文中当前与其他进程共享的部分。
///
/// flags 为 0 时调用成功并立即返回。
/// 当前内核不支持任何 namespace (CLONE_NEW*) 或 CLONE_FILES/CLONE_FS
/// 的解除共享操作，传入不支持的标志将返回 EINVAL。
pub fn sys_unshare(flags: i32) -> SyscallRet {
    debug!("[sys_unshare] flags=0x{:x}", flags);

    // flags == 0 is a valid no-op: the caller simply queries whether
    // unshare is available without requesting any actual unsharing.
    if flags == 0 {
        return Ok(0);
    }

    // Any non-zero flag combination is currently unsupported because
    // this kernel has neither namespace infrastructure nor the ability
    // to reverse CLONE_FILES / CLONE_FS sharing.
    Err(SysErrNo::EINVAL)
}
