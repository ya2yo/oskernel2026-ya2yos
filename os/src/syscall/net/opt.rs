use log::warn;

use crate::utils::SyscallRet;

/// 参考 https://man7.org/linux/man-pages/man2/setsockopt.2.html
pub fn sys_setsockopt(
    _sockfd: usize,
    _level: u32,
    _optname: u32,
    _optcal: *const u8,
    _optlen: u32,
) -> SyscallRet {
    warn!("[sys_setsockopt] fd={}", _sockfd,);
    warn!("sys_setsockopt is not implemented, return Ok(0)");

    Ok(0)
}