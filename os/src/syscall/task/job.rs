use log::debug;

use crate::{
    task::{current_task, Process},
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/setsid.2.html
pub fn sys_setsid() -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut meta = task.process.meta_lock();
    meta.pgid = task.pid();
    debug!("[sys_setsid] pid {} new pgid {}", task.pid(), meta.pgid);
    Ok(meta.pgid)
}

/// 参考 https://man7.org/linux/man-pages/man2/getpgid.2.html
pub fn sys_getpgid(pid: u32) -> SyscallRet {
    let target = if pid == 0 {
        current_task().ok_or(SysErrNo::ESRCH)?.process.clone()
    } else {
        Process::get_process_arc_by_pid(pid as usize).ok_or(SysErrNo::ESRCH)?
    };
    Ok(target.pgid())
}

/// https://www.man7.org/linux/man-pages/man2/setpgid.2.html
pub fn sys_setpgid(pid: u32, pgid: u32) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let target_pid = if pid == 0 { task.pid() } else { pid as usize };
    let new_pgid = if pgid == 0 { target_pid } else { pgid as usize };
    let target = Process::get_process_arc_by_pid(target_pid).ok_or(SysErrNo::ESRCH)?;
    let parent_pid = target.meta_lock().parent_pid;
    if target_pid != task.pid() && parent_pid != task.pid() {
        return Err(SysErrNo::ESRCH);
    }
    target.meta_lock().pgid = new_pgid;
    debug!("[sys_setpgid] pid {} pgid {}", target_pid, new_pgid);
    Ok(0)
}
