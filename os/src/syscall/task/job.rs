use log::warn;

use crate::utils::SyscallRet;

/// 参考 https://man7.org/linux/man-pages/man2/setsid.2.html
pub fn sys_setsid() -> SyscallRet {
    warn!("[sys_setsid] We do not really support process group!");
    Ok(0)
}

pub fn sys_getpgid() -> SyscallRet {
    warn!("[sys_getpgid] We do not really support process group!");
    Ok(0)
}

pub fn sys_setpgid() -> SyscallRet {
    warn!("[sys_setpgid] We do not really support process group!");
    Ok(0)
}