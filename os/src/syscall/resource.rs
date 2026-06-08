use crate::{
    mm::{copy_from_user, copy_to_user},
    syscall::RLimit,
    task::current_task,
    utils::SyscallRet,
};

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
        let inner = task.process.inner_lock();
        let memory_set = inner.get_locked_memory_set_read();
        let fd_table = inner.fd_table.clone();
        if !old_limit.is_null() {
            // 说明是get
            let limit = RLimit {
                rlim_cur: fd_table.get_soft_limit(),
                rlim_max: fd_table.get_hard_limit(),
            };
            copy_to_user(&memory_set, old_limit as usize, unsafe {
                core::slice::from_raw_parts(
                    &limit as *const RLimit as *const u8,
                    core::mem::size_of::<RLimit>(),
                )
            })?;
        }
        if !new_limit.is_null() {
            // 说明是set
            let mut limit = RLimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            copy_from_user(&memory_set, new_limit as usize, unsafe {
                core::slice::from_raw_parts_mut(
                    &mut limit as *mut RLimit as *mut u8,
                    core::mem::size_of::<RLimit>(),
                )
            })?;
            fd_table.set_limit(limit.rlim_cur, limit.rlim_max);
        }
    } else {
        unimplemented!("pid must equal zero");
    }

    Ok(0)
}
