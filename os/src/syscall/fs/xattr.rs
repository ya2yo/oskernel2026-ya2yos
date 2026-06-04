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

pub fn sys_fsetxattr(fd: usize, name: usize, value: usize, size: usize, flags: usize) -> SyscallRet {
    debug!("[sys_fsetxattr] path={}, name={}, value={}, size={}, flag={}", fd, name, value, size, flags);
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
pub fn sys_fgetxattr(fd: usize, name: usize, value: usize, size: usize) -> SyscallRet {
    debug!("[sys_fgetxattr] path={}, name={}, value={}, size={}", fd, name, value, size);
    Ok(0)
}

/// https://www.man7.org/linux/man-pages/man2/listxattr.2.html
pub fn sys_listxattr(path: usize, list: usize, size: usize)->SyscallRet {
    debug!("[sys_listxattr] path={}, list={}, size={}", path, list, size);
    Ok(0)
}
pub fn sys_llistxattr(path: usize, list: usize, size: usize)->SyscallRet {
    debug!("[sys_llistxattr] path={}, list={}, size={}", path, list, size);
    Ok(0)
}
pub fn sys_flistxattr(fd: usize, list: usize, size: usize)->SyscallRet {
    debug!("[sys_flistxattr] path={}, list={}, size={}", fd, list, size);
    Ok(0)
}
/// https://www.man7.org/linux/man-pages/man2/removexattr.2.html
pub fn sys_removexattr(path: usize, name: usize)->SyscallRet {
    debug!("[sys_removexattr] path={}, name={}", path, name);
    Ok(0)
}
pub fn sys_lremovexattr(path: usize, name: usize)->SyscallRet {
    debug!("[sys_lremovexattr] path={}, name={}", path, name);
    Ok(0)
}
pub fn sys_femovexattr(fd: usize, name: usize)->SyscallRet {
    debug!("[sys_fremovexattr] path={}, name={}", fd, name);
    Ok(0)
}