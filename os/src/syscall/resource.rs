use crate::{
    mm::{copy_from_user, copy_to_user},
    syscall::{
        task::{default_rlimit, RLIMIT_NOFILE},
        RLimit,
    },
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/prlimit64.2.html
pub fn sys_prlimit(
    pid: usize,
    resource: u32,
    new_limit: *const RLimit,
    old_limit: *mut RLimit,
) -> SyscallRet {
    if pid != 0 {
        return Err(SysErrNo::ESRCH);
    }

    let task = current_task().unwrap();
    let inner = task.process.inner_lock();
    let memory_set = inner.get_locked_memory_set_read();
    let fd_table = inner.fd_table.clone();
    if !old_limit.is_null() {
        let limit = if resource as i32 == RLIMIT_NOFILE {
            RLimit {
                rlim_cur: fd_table.get_soft_limit(),
                rlim_max: fd_table.get_hard_limit(),
            }
        } else {
            default_rlimit(resource as i32)
        };
        copy_to_user(&memory_set, old_limit as usize, unsafe {
            core::slice::from_raw_parts(
                &limit as *const RLimit as *const u8,
                core::mem::size_of::<RLimit>(),
            )
        })?;
    }
    if !new_limit.is_null() {
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
        if limit.rlim_cur > limit.rlim_max {
            return Err(SysErrNo::EINVAL);
        }
        if resource as i32 == RLIMIT_NOFILE {
            fd_table.set_limit(limit.rlim_cur, limit.rlim_max);
        }
    }

    Ok(0)
}
