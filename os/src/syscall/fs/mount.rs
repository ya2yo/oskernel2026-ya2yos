use alloc::string::String;
use log::warn;

use crate::{
    fs::MNT_TABLE, mm::translated_str, syscall::fs::dummyfd_create, task::current_token, utils::{SysErrNo, SyscallRet}
};

/// 参考 https://man7.org/linux/man-pages/man2/umount2.2.html
pub fn sys_umount2(special: *const u8, flags: u32) -> SyscallRet {
    let token = current_token();
    let special = translated_str(token, special);

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
    const MS_RDONLY: u32 = 1;
    const MS_REMOUNT: u32 = 32;

    let token = current_token();
    let special = translated_str(token, special);
    let dir = translated_str(token, dir);
    let ftype = translated_str(token, ftype);
    //log::info!("flags = {}", flags);
    if !data.is_null() {
        let data = translated_str(token, data);
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
pub fn sys_fsopen(_fsname: *const u8, _flags: u32)->SyscallRet {
    warn!("[sys_fsopen] not implement!");
    dummyfd_create()
}
/// https://man7.org/linux/man-pages/man2/fspick.2.html
pub fn sys_fspick(_dirfd: i32, _path: *mut u8, _flags: u32) -> SyscallRet {
    warn!("[sys_fspick] not implement!");
    dummyfd_create()
}