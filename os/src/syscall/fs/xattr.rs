//! Linux extended-attribute syscalls.
//!
//! The syscall layer owns user pointers and pathname/fd resolution.  The
//! filesystem backend owns xattr state and, for EXT4, keeps the operation
//! under the mount-wide lwext4 gate.

use alloc::{sync::Arc, vec::Vec};

use crate::{
    fs::{open, Inode, OpenFlags, MAX_PATH_LEN, NONE_MODE},
    mm::{copy_from_user, copy_to_user, read_user_cstr, read_user_cstr_with_limit, MemorySet},
    task::{current_task, Process},
    utils::{SysErrNo, SyscallRet},
};

const XATTR_CREATE: u32 = 0x1;
const XATTR_REPLACE: u32 = 0x2;
const XATTR_NAME_MAX: usize = 255;
const XATTR_VALUE_MAX: usize = 65_536;
const XATTR_LIST_MAX: usize = 65_536;

fn xattr_name_from_user(memory_set: &MemorySet, name: usize) -> Result<Vec<u8>, SysErrNo> {
    if name == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let name = read_user_cstr_with_limit(memory_set, name as *const u8, XATTR_NAME_MAX + 1)
        .map_err(|err| {
            if err == SysErrNo::E2BIG {
                SysErrNo::ERANGE
            } else {
                err
            }
        })?;
    if name.is_empty() {
        return Err(SysErrNo::ERANGE);
    }
    Ok(name)
}

fn xattr_path_from_user(
    memory_set: &MemorySet,
    path: usize,
) -> Result<alloc::string::String, SysErrNo> {
    if path == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let path = read_user_cstr(memory_set, path as *const u8)?;
    if path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    if path.len() >= MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }
    Ok(path)
}

fn inode_from_path(
    proc: &Process,
    memory_set: &MemorySet,
    path: usize,
    nofollow: bool,
) -> Result<Arc<dyn Inode>, SysErrNo> {
    let path = xattr_path_from_user(memory_set, path)?;
    let path = proc.get_abs_path(-100, &path)?;
    let flags = if nofollow {
        // `O_NOFOLLOW` reports ELOOP for a final symlink.  The VFS-internal
        // flag instead returns that symlink inode, which is exactly what the
        // Linux l* xattr operations need.
        OpenFlags::O_RDONLY | OpenFlags::O_UNLINK
    } else {
        OpenFlags::O_RDONLY
    };
    open(&path, flags, NONE_MODE)
        .and_then(|file| file.file())
        .map(|file| file.inode.clone())
        .map_err(|err| {
            if err == SysErrNo::EINVAL {
                SysErrNo::EOPNOTSUPP
            } else {
                err
            }
        })
}

fn inode_from_fd(proc: &Process, fd: usize) -> Result<Arc<dyn Inode>, SysErrNo> {
    let fd_desc = proc.fd_table.get(fd)?;
    // Linux xattr fd operations require a normal opened file; O_PATH only
    // carries a pathname and therefore fails with EBADF.
    if fd_desc.is_path_only() {
        return Err(SysErrNo::EBADF);
    }
    fd_desc
        .file()
        .map(|file| file.inode.clone())
        .map_err(|err| {
            if err == SysErrNo::EINVAL {
                SysErrNo::EOPNOTSUPP
            } else {
                err
            }
        })
}

fn copy_xattr_value_from_user(
    memory_set: &MemorySet,
    value: usize,
    size: usize,
) -> Result<Vec<u8>, SysErrNo> {
    if size > XATTR_VALUE_MAX {
        return Err(SysErrNo::E2BIG);
    }
    if size == 0 {
        return Ok(Vec::new());
    }
    if value == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let mut copied = Vec::new();
    copied
        .try_reserve_exact(size)
        .map_err(|_| SysErrNo::ENOMEM)?;
    copied.resize(size, 0);
    copy_from_user(memory_set, value, copied.as_mut_slice())?;
    Ok(copied)
}

fn output_buffer(size: usize, max_size: usize) -> Result<Vec<u8>, SysErrNo> {
    let size = size.min(max_size);
    let mut output = Vec::new();
    output
        .try_reserve_exact(size)
        .map_err(|_| SysErrNo::ENOMEM)?;
    output.resize(size, 0);
    Ok(output)
}

fn copy_xattr_result_to_user(memory_set: &MemorySet, value: usize, output: &[u8]) -> SyscallRet {
    if output.is_empty() {
        return Ok(0);
    }
    if value == 0 {
        return Err(SysErrNo::EFAULT);
    }
    copy_to_user(memory_set, value, output).map(|_| 0)
}

fn checked_xattr_flags(flags: usize) -> Result<u32, SysErrNo> {
    if flags > u32::MAX as usize {
        return Err(SysErrNo::EINVAL);
    }
    let flags = flags as u32;
    if flags & !(XATTR_CREATE | XATTR_REPLACE) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    Ok(flags)
}

fn set_xattr_path(
    path: usize,
    name: usize,
    value: usize,
    size: usize,
    flags: usize,
    nofollow: bool,
) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let name = xattr_name_from_user(&memory_set, name)?;
    let value = copy_xattr_value_from_user(&memory_set, value, size)?;
    let flags = checked_xattr_flags(flags)?;
    let inode = inode_from_path(proc, &memory_set, path, nofollow)?;
    inode.set_xattr(&name, &value, flags).map(|_| 0)
}

fn set_xattr_fd(fd: usize, name: usize, value: usize, size: usize, flags: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let name = xattr_name_from_user(&memory_set, name)?;
    let value = copy_xattr_value_from_user(&memory_set, value, size)?;
    let flags = checked_xattr_flags(flags)?;
    let inode = inode_from_fd(proc, fd)?;
    inode.set_xattr(&name, &value, flags).map(|_| 0)
}

fn get_xattr_path(
    path: usize,
    name: usize,
    value: usize,
    size: usize,
    nofollow: bool,
) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let name = xattr_name_from_user(&memory_set, name)?;
    let inode = inode_from_path(proc, &memory_set, path, nofollow)?;
    let mut output = output_buffer(size, XATTR_VALUE_MAX)?;
    let result = inode.get_xattr(&name, output.as_mut_slice());
    let len = match result {
        Err(SysErrNo::ERANGE) if size >= XATTR_VALUE_MAX => return Err(SysErrNo::E2BIG),
        Err(err) => return Err(err),
        Ok(len) => len,
    };
    if len > output.len() {
        return Err(SysErrNo::ERANGE);
    }
    copy_xattr_result_to_user(&memory_set, value, &output[..len])?;
    Ok(len)
}

fn get_xattr_fd(fd: usize, name: usize, value: usize, size: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let name = xattr_name_from_user(&memory_set, name)?;
    let inode = inode_from_fd(proc, fd)?;
    let mut output = output_buffer(size, XATTR_VALUE_MAX)?;
    let result = inode.get_xattr(&name, output.as_mut_slice());
    let len = match result {
        Err(SysErrNo::ERANGE) if size >= XATTR_VALUE_MAX => return Err(SysErrNo::E2BIG),
        Err(err) => return Err(err),
        Ok(len) => len,
    };
    if len > output.len() {
        return Err(SysErrNo::ERANGE);
    }
    copy_xattr_result_to_user(&memory_set, value, &output[..len])?;
    Ok(len)
}

fn list_xattr_path(path: usize, list: usize, size: usize, nofollow: bool) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let inode = inode_from_path(proc, &memory_set, path, nofollow)?;
    let mut output = output_buffer(size, XATTR_LIST_MAX)?;
    let result = inode.list_xattr(output.as_mut_slice());
    let len = match result {
        Err(SysErrNo::ERANGE) if size >= XATTR_LIST_MAX => return Err(SysErrNo::E2BIG),
        Err(err) => return Err(err),
        Ok(len) => len,
    };
    if len > output.len() {
        return Err(SysErrNo::ERANGE);
    }
    copy_xattr_result_to_user(&memory_set, list, &output[..len])?;
    Ok(len)
}

fn list_xattr_fd(fd: usize, list: usize, size: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let inode = inode_from_fd(proc, fd)?;
    let mut output = output_buffer(size, XATTR_LIST_MAX)?;
    let result = inode.list_xattr(output.as_mut_slice());
    let len = match result {
        Err(SysErrNo::ERANGE) if size >= XATTR_LIST_MAX => return Err(SysErrNo::E2BIG),
        Err(err) => return Err(err),
        Ok(len) => len,
    };
    if len > output.len() {
        return Err(SysErrNo::ERANGE);
    }
    copy_xattr_result_to_user(&memory_set, list, &output[..len])?;
    Ok(len)
}

fn remove_xattr_path(path: usize, name: usize, nofollow: bool) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let name = xattr_name_from_user(&memory_set, name)?;
    let inode = inode_from_path(proc, &memory_set, path, nofollow)?;
    inode.remove_xattr(&name).map(|_| 0)
}

fn remove_xattr_fd(fd: usize, name: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let name = xattr_name_from_user(&memory_set, name)?;
    let inode = inode_from_fd(proc, fd)?;
    inode.remove_xattr(&name).map(|_| 0)
}

/// `setxattr(2)`: follow the final symbolic link.
pub fn sys_setxattr(
    path: usize,
    name: usize,
    value: usize,
    size: usize,
    flags: usize,
) -> SyscallRet {
    set_xattr_path(path, name, value, size, flags, false)
}

/// `lsetxattr(2)`: operate on the final symbolic link itself.
pub fn sys_lsetxattr(
    path: usize,
    name: usize,
    value: usize,
    size: usize,
    flags: usize,
) -> SyscallRet {
    set_xattr_path(path, name, value, size, flags, true)
}

pub fn sys_fsetxattr(
    fd: usize,
    name: usize,
    value: usize,
    size: usize,
    flags: usize,
) -> SyscallRet {
    set_xattr_fd(fd, name, value, size, flags)
}

/// `getxattr(2)`: follow the final symbolic link.
pub fn sys_getxattr(path: usize, name: usize, value: usize, size: usize) -> SyscallRet {
    get_xattr_path(path, name, value, size, false)
}

pub fn sys_lgetxattr(path: usize, name: usize, value: usize, size: usize) -> SyscallRet {
    get_xattr_path(path, name, value, size, true)
}

pub fn sys_fgetxattr(fd: usize, name: usize, value: usize, size: usize) -> SyscallRet {
    get_xattr_fd(fd, name, value, size)
}

pub fn sys_listxattr(path: usize, list: usize, size: usize) -> SyscallRet {
    list_xattr_path(path, list, size, false)
}

pub fn sys_llistxattr(path: usize, list: usize, size: usize) -> SyscallRet {
    list_xattr_path(path, list, size, true)
}

pub fn sys_flistxattr(fd: usize, list: usize, size: usize) -> SyscallRet {
    list_xattr_fd(fd, list, size)
}

pub fn sys_removexattr(path: usize, name: usize) -> SyscallRet {
    remove_xattr_path(path, name, false)
}

pub fn sys_lremovexattr(path: usize, name: usize) -> SyscallRet {
    remove_xattr_path(path, name, true)
}

pub fn sys_fremovexattr(fd: usize, name: usize) -> SyscallRet {
    remove_xattr_fd(fd, name)
}
