use alloc::string::String;
use log::{debug, warn};

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

/// https://www.man7.org/linux/man-pages/man2/fsconfig.2.html
///
/// 配置/创建由 fsopen(2) 或 fspick(2) 获得的文件系统上下文。
///
/// 命令:
/// - FSCONFIG_SET_FLAG   (0): 设置标志位选项
/// - FSCONFIG_SET_STRING (1): 设置字符串选项
/// - FSCONFIG_SET_BINARY (2): 设置二进制选项 (aux = 数据长度)
/// - FSCONFIG_SET_PATH   (3): 设置路径选项
/// - FSCONFIG_SET_PATH_EMPTY (4): 设置空路径选项
/// - FSCONFIG_SET_FD     (5): 设置 fd 选项, value 是 fd 编号
/// - FSCONFIG_CMD_CREATE  (6): 创建文件系统/superblock
/// - FSCONFIG_CMD_RECONFIGURE (7): 重新配置文件系统参数
pub fn sys_fsconfig(fd: i32, cmd: u32, key: usize, value: usize, aux: i32) -> SyscallRet {
    const FSCONFIG_SET_FLAG: u32 = 0;
    const FSCONFIG_SET_STRING: u32 = 1;
    const FSCONFIG_SET_BINARY: u32 = 2;
    const FSCONFIG_SET_PATH: u32 = 3;
    const FSCONFIG_SET_PATH_EMPTY: u32 = 4;
    const FSCONFIG_SET_FD: u32 = 5;
    const FSCONFIG_CMD_CREATE: u32 = 6;
    const FSCONFIG_CMD_RECONFIGURE: u32 = 7;

    // EBADF: fd 无效
    if fd < 0 {
        return Err(SysErrNo::EBADF);
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    // 验证 fd 存在（fsopen/fspick 返回的 dummy fd）
    let _ = proc_inner.fd_table.get(fd as usize)?;

    match cmd {
        FSCONFIG_SET_FLAG => {
            // key 是标志名（字符串），value 忽略
            if key != 0 {
                let _key_str = read_user_cstr(&memory_set, key as *const u8)?;
            }
            Ok(0)
        }
        FSCONFIG_SET_STRING => {
            // key 是选项名，value 是选项值（字符串）
            if key != 0 {
                let _key_str = read_user_cstr(&memory_set, key as *const u8)?;
            }
            if value != 0 {
                let _val_str = read_user_cstr(&memory_set, value as *const u8)?;
            }
            Ok(0)
        }
        FSCONFIG_SET_BINARY => {
            // key 是选项名，value 是二进制数据（aux 字节长度）
            if key != 0 {
                let _key_str = read_user_cstr(&memory_set, key as *const u8)?;
            }
            if value != 0 && aux > 0 {
                // 通过 copy_from_user 验证用户空间内存可读
                let len = core::cmp::min(aux as usize, 256);
                let mut _buf = alloc::vec![0u8; len];
                copy_from_user(&memory_set, value, &mut _buf[..])?;
            }
            Ok(0)
        }
        FSCONFIG_SET_PATH | FSCONFIG_SET_PATH_EMPTY => {
            // key 是选项名，value 是路径字符串
            if key != 0 {
                let _key_str = read_user_cstr(&memory_set, key as *const u8)?;
            }
            if value != 0 {
                let _path = read_user_cstr(&memory_set, value as *const u8)?;
            }
            Ok(0)
        }
        FSCONFIG_SET_FD => {
            // key 是选项名，value 是需要传入的 fd 编号
            if key != 0 {
                let _key_str = read_user_cstr(&memory_set, key as *const u8)?;
            }
            let target_fd = value;
            // 验证传入的 fd 有效
            let _ = proc_inner.fd_table.get(target_fd)?;
            Ok(0)
        }
        FSCONFIG_CMD_CREATE => {
            // 在完整实现中，这里会使用累积的配置创建 superblock
            // 当前简化实现直接返回成功
            debug!("[sys_fsconfig] FSCONFIG_CMD_CREATE: mount commit (stub)");
            Ok(0)
        }
        FSCONFIG_CMD_RECONFIGURE => {
            debug!("[sys_fsconfig] FSCONFIG_CMD_RECONFIGURE (stub)");
            Ok(0)
        }
        _ => {
            // 无效命令
            Err(SysErrNo::EINVAL)
        }
    }
}

/// https://man7.org/linux/man-pages/man2/fspick.2.html
pub fn sys_fspick(_dirfd: i32, _path: *mut u8, _flags: u32) -> SyscallRet {
    warn!("[sys_fspick] not implement!");
    dummyfd_create()
}
