use log::debug;

use crate::{
    fs::{open, InodeType, OpenFlags, NONE_MODE},
    mm::{if_bad_address, read_user_cstr},
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/chroot.2.html
pub fn sys_chroot(path: *const u8) -> SyscallRet {
    debug!("[chroot] path=0x{:x}", path as usize);

    let task = current_task().unwrap();

    if path.is_null() {
        return Err(SysErrNo::EINVAL);
    }
    if (path as isize) <= 0 || if_bad_address(path as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if task.inner_lock().user_id != 0 {
        return Err(SysErrNo::EPERM);
    }

    let path_str = {
        let proc_inner = &task.process;
        let memory_set = proc_inner.memory_set_arc();
        read_user_cstr(&memory_set, path)?
    };

    let file = open(&path_str, OpenFlags::O_RDONLY, NONE_MODE)?;
    let osfile = file.file()?;
    if osfile.inode.types() != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }

    task.process.fs_info.set_cwd(path_str);
    debug!("[chroot] success");
    Ok(0)
}
