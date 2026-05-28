use log::{debug, warn};

use crate::{
    mm::put_data,
    task::{current_task, tid_to_task},
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/set_robust_list.2.html
pub fn sys_set_robust_list(head: usize, len: usize) -> SyscallRet {
    if len != crate::task::HEAD_SIZE {
        warn!("sys_set_robust_list len != HEAD_SIZE. early return");
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    task_inner.robust_list.head = head;
    task_inner.robust_list.len = len; // 要不把它取消注释了？
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
        let token = task
            .process
            .inner_lock()
            .get_locked_memory_set_read()
            .token();
        put_data(token, head_ptr, task_inner.robust_list.head);
        put_data(token, len_ptr, task_inner.robust_list.len);
        Ok(0)
    } else {
        Err(SysErrNo::ESRCH)
    }
}
