use crate::{
    fs::{open, FileClass, InodeType, OpenFlags, MAX_PATH_LEN, NONE_MODE},
    mm::{
        get_data, if_bad_address, put_data, read_user_cstr, safe_put_data, translated_ref, VirtAddr,
    },
    signal::{check_if_any_sig_for_current_task, handle_signal},
    syscall::{CloneFlags, Utsname},
    task::{
        current_task, current_token, exit_current_and_run_next, exit_current_group_and_run_next,
        futex_wake_up, ready_queue, suspend_current_and_run_next, tid_to_task, Process, Processor,
        Sysinfo,
    },
    timer::{calculate_left_timespec, get_time_ms, get_time_spec, Timespec},
    utils::{get_abs_path, strip_color, trim_start_slash, SysErrNo, SyscallRet},
};
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use spin::{Lazy, Mutex};

use log::{debug, error, warn};

/// 全局进程记账文件
/// acct(2) 系统调用用于开启/关闭进程记账。
/// 开启时把生成的记账记录写入指定文件；关闭时传入 NULL。
static ACCT_FILE: Lazy<Mutex<Option<Arc<crate::fs::OSFile>>>> =
    Lazy::new(|| Mutex::new(None));

/// 参考 https://man7.org/linux/man-pages/man2/acct.2.html
pub fn sys_acct(filename: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let uid = task_inner.user_id;
    drop(task_inner);

    // NULL 表示关闭记账
    if filename.is_null() {
        // 非 root 用户无权关闭记账
        if uid != 0 {
            return Err(SysErrNo::EPERM);
        }
        *ACCT_FILE.lock() = None;
        debug!("[sys_acct] accounting disabled");
        return Ok(0);
    }

    // 只有 root 用户可以开启记账
    if uid != 0 {
        return Err(SysErrNo::EPERM);
    }

    // 检查用户空间地址合法性
    if (filename as isize) <= 0 || if_bad_address(filename as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let path = read_user_cstr(&memory_set, filename)?;

    // 检查路径长度
    if path.len() > MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    // 计算绝对路径
    let abs_path = get_abs_path(&proc_inner.fs_info.get_cwd(), &path);
    debug!("[sys_acct] filename = {}, abs_path = {}", path, abs_path);
    drop(memory_set);
    drop(proc_inner);

    // 尝试打开文件以验证路径有效
    let file_class = open(&abs_path, OpenFlags::O_WRONLY, NONE_MODE)?;

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

    // 存储记账文件
    *ACCT_FILE.lock() = Some(osfile);
    debug!("[sys_acct] accounting enabled, file = {}", abs_path);
    Ok(0)
}
