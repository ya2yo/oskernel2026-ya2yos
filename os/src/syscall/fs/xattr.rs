use log::debug;

use crate::utils::SyscallRet;

/// 参考 https://www.man7.org/linux/man-pages/man2/fsetxattr.2.html
pub fn sys_setxattr(path: usize, name: usize, value: usize, size: usize, flags: usize) -> SyscallRet {
    debug!("[sys_setxattr] path={}, name={}, value={}, size={}, flag={}", path, name, value, size, flags);
    Ok(0)
}

pub fn sys_lsetxattr(path: usize, name: usize, value: usize, size: usize, flags: usize) -> SyscallRet {
    debug!("[sys_lsetxattr] path={}, name={}, value={}, size={}, flag={}", path, name, value, size, flags);
    Ok(0)
}

pub fn sys_fsetxattr(path: usize, name: usize, value: usize, size: usize, flags: usize) -> SyscallRet {
    debug!("[sys_fsetxattr] path={}, name={}, value={}, size={}, flag={}", path, name, value, size, flags);
    Ok(0)
}

/// https://www.man7.org/linux/man-pages/man2/getxattr.2.html
pub fn sys_getxattr(path: usize, name: usize, value: usize, size: usize)->SyscallRet {
    debug!("[sys_getxattr] path={}, name={}, value={}, size={}", path, name, value, size);
    Ok(0)
}
pub fn sys_lgetxattr(path: usize, name: usize, value: usize, size: usize) -> SyscallRet {
    debug!("[sys_lgetxattr] path={}, name={}, value={}, size={}", path, name, value, size);
    Ok(0)
}
pub fn sys_fgetxattr(path: usize, name: usize, value: usize, size: usize) -> SyscallRet {
    debug!("[sys_fgetxattr] path={}, name={}, value={}, size={}", path, name, value, size);
    Ok(0)
}