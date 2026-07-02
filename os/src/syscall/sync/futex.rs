use log::{debug, warn};

use crate::{
    mm::copy_to_user,
    task::{current_task, tid_to_task, RobustListHead},
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/set_robust_list.2.html
pub fn sys_set_robust_list(head: usize, len: usize) -> SyscallRet {
    if len != core::mem::size_of::<RobustListHead>() {
        warn!("sys_set_robust_list len != HEAD_SIZE. early return");
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    task_inner.robust_list.list = head;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/get_robust_list.2.html
pub fn sys_get_robust_list(pid: usize, head_ptr: *mut usize, len_ptr: *mut usize) -> SyscallRet {
    let mut task = tid_to_task::tid2task(pid);
    if task.is_none() && pid == 0 {
        task = current_task();
    }
    if let Some(task) = task {
        let task_inner = task.inner_lock();
        let proc_inner = &task.process;
        let memory_set = proc_inner.get_locked_memory_set_read();
        let head_val = task_inner.robust_list.list;
        copy_to_user(&memory_set, head_ptr as usize, unsafe {
            core::slice::from_raw_parts(
                &head_val as *const usize as *const u8,
                core::mem::size_of::<usize>(),
            )
        })?;
        let len_val = core::mem::size_of::<RobustListHead>();
        copy_to_user(&memory_set, len_ptr as usize, unsafe {
            core::slice::from_raw_parts(
                &len_val as *const usize as *const u8,
                core::mem::size_of::<usize>(),
            )
        })?;
        Ok(0)
    } else {
        Err(SysErrNo::ESRCH)
    }
}
