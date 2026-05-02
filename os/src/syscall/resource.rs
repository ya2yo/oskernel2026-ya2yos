use crate::{mm::{translated_ref, translated_refmut}, syscall::RLimit, task::current_task, utils::SyscallRet};

/// 参考 https://man7.org/linux/man-pages/man2/prlimit64.2.html
pub fn sys_prlimit(
    pid: usize,
    resource: u32,
    new_limit: *const RLimit,
    old_limit: *mut RLimit,
) -> SyscallRet {
    const RLIMIT_NOFILE: u32 = 7;
    if resource != RLIMIT_NOFILE {
        return Ok(0);
    }

    if pid == 0 {
        let task = current_task().unwrap();
        let mut inner = task.process.inner_lock();
        let token = task.process.inner_lock().get_locked_memory_set_read().token();
        let fd_table = &mut inner.fd_table;
        if !old_limit.is_null() {
            // 说明是get
            let limit = translated_refmut(token, old_limit);
            limit.rlim_cur = fd_table.get_soft_limit();
            limit.rlim_max = fd_table.get_hard_limit();
        }
        if !new_limit.is_null() {
            // 说明是set
            let limit: &RLimit = translated_ref(token, new_limit);
            fd_table.set_limit(limit.rlim_cur, limit.rlim_max);
        }
    } else {
        unimplemented!("pid must equal zero");
    }

    Ok(0)
}