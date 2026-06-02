use alloc::string::String;
use log::warn;

use crate::{
    fs::{MAX_PATH_LEN, MNT_TABLE},
    mm::{MemorySet, copy_from_user, translate::read_user_cstr},
    syscall::fs::dummyfd_create,
    task::{current_task, current_token},
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/pivot_root.2.html
pub fn sys_pivot_root(_new_root: usize, _put_old: usize) -> SyscallRet {
    warn!("[sys_pivot_root] not implement!");
    /*
       EBUSY  new_root or put_old is on the current root mount.  (This
              error covers the pathological case where new_root is "/".)
       EINVAL new_root is not a mount point.
       EINVAL put_old is not at or underneath new_root.
       EINVAL The current root directory is not a mount point (because of
              an earlier chroot(2)).
       EINVAL The current root is on the rootfs (initial ramfs) mount;
              see NOTES.
       EINVAL Either the mount point at new_root, or the parent mount of
              that mount point, has propagation type MS_SHARED.
       EINVAL put_old is a mount point and has the propagation type
              MS_SHARED.
       ENOTDIR
              new_root or put_old is not a directory.
       EPERM  The calling process does not have the CAP_SYS_ADMIN
              capability. 
    */
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/umount2.2.html
pub fn sys_umount2(special: *const u8, flags: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let special = read_user_cstr(&memory_set, special)?;

    let ret = MNT_TABLE.lock().umount(special, flags);
    if ret != -1 {
        Ok(0)
    } else {
        Err(SysErrNo::EINVAL)
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/mount.2.html
pub fn sys_mount(
    special: *const u8,
    dir: *const u8,
    ftype: *const u8,
    flags: u32,
    data: *const u8,
) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let special = read_user_cstr(&memory_set, special)?;
    let dir = read_user_cstr(&memory_set, dir)?;
    let ftype = read_user_cstr(&memory_set, ftype)?;
    if !data.is_null() {
        let data = read_user_cstr(&memory_set, data)?;
        let ret = MNT_TABLE.lock().mount(special, dir, ftype, flags, data);
        if ret != -1 {
            Ok(0)
        } else {
            Err(SysErrNo::ENOSPC)
        }
    } else {
        let ret = MNT_TABLE
            .lock()
            .mount(special, dir, ftype, flags, String::from(""));
        if ret != -1 {
            Ok(0)
        } else {
            Err(SysErrNo::ENOSPC)
        }
    }
}

/// https://man7.org/linux/man-pages/man2/open_tree.2.html
pub fn sys_open_tree(_dirfd: i32, _path: *const u8, _flags: u32) -> SyscallRet {
    warn!("[sys_open_tree] not implement!");
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/fsopen.2.html
pub fn sys_fsopen(_fsname: *const u8, _flags: u32) -> SyscallRet {
    warn!("[sys_fsopen] not implement!");
    dummyfd_create()
}
/// https://man7.org/linux/man-pages/man2/fspick.2.html
pub fn sys_fspick(_dirfd: i32, _path: *mut u8, _flags: u32) -> SyscallRet {
    warn!("[sys_fspick] not implement!");
    dummyfd_create()
}
