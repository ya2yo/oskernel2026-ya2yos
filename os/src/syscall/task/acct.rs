use crate::{
    fs::{open, FileClass, InodeType, MountFlags, OpenFlags, MAX_PATH_LEN, MNT_TABLE, NONE_MODE},
    mm::{if_bad_address, read_user_cstr},
    task::{current_task, set_process_acct_file},
    utils::{get_abs_path, SysErrNo, SyscallRet},
};

use log::debug;

const MAX_FILE_NAME_LEN: usize = 255;

fn has_too_long_path_component(path: &str) -> bool {
    path.split('/')
        .any(|component| component.len() > MAX_FILE_NAME_LEN)
}

/// 参考 https://man7.org/linux/man-pages/man2/acct.2.html
pub fn sys_acct(filename: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let effective_uid = task_inner.effective_uid;
    drop(task_inner);

    // NULL 表示关闭记账
    if filename.is_null() {
        // 非 root 用户无权关闭记账
        if effective_uid != 0 {
            return Err(SysErrNo::EPERM);
        }
        set_process_acct_file(None);
        debug!("[sys_acct] accounting disabled");
        return Ok(0);
    }

    // 只有 root 用户可以开启记账
    if effective_uid != 0 {
        return Err(SysErrNo::EPERM);
    }

    // 检查用户空间地址合法性
    if (filename as isize) <= 0 || if_bad_address(filename as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
    let path = read_user_cstr(&memory_set, filename)?;

    if path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }

    // 检查路径长度
    if path.len() > MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }
    if has_too_long_path_component(&path) {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    // 计算绝对路径
    let abs_path = get_abs_path(&proc_inner.fs_info.get_cwd(), &path);
    debug!("[sys_acct] filename = {}, abs_path = {}", path, abs_path);
    drop(memory_set);

    if path.ends_with('/') && path != "/" {
        open(
            &abs_path,
            OpenFlags::O_RDONLY | OpenFlags::O_DIRECTORY,
            NONE_MODE,
        )?;
        return Err(SysErrNo::EISDIR);
    }

    // 先只读打开以验证路径和类型，避免写权限检查覆盖 acct 自身的 errno 语义。
    let file_class = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?;

    // 必须为普通文件，不能为设备、socket 等
    let osfile = match &file_class {
        FileClass::File(f) => f.clone(),
        _ => {
            // 非普通文件（设备、socket 等）返回 EACCES
            return Err(SysErrNo::EACCES);
        }
    };

    // 检查是否为目录
    if osfile.inode.types().is_dir() {
        return Err(SysErrNo::EISDIR);
    }

    // 检查是否为普通文件
    if !osfile.inode.types().is_file() {
        return Err(SysErrNo::EACCES);
    }

    if let Some((_, _, _, mountflags)) = MNT_TABLE.lock().mount_for_path(&abs_path) {
        if mountflags.contains(MountFlags::RDONLY) {
            return Err(SysErrNo::EROFS);
        }
    }

    let osfile = open(&abs_path, OpenFlags::O_WRONLY, NONE_MODE)?.file()?;

    set_process_acct_file(Some(osfile));
    debug!("[sys_acct] accounting enabled, file = {}", abs_path);
    Ok(0)
}
