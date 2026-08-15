use crate::{
    mm::{copy_from_user, copy_to_user, if_bad_address},
    syscall::RLimit,
    task::{current_task, tid_to_task, Process, TaskControlBlock},
    utils::{SysErrNo, SyscallRet},
};

// ---------------------------------------------------------------------------
// getpriority(141) / setpriority(140)
// ---------------------------------------------------------------------------

/// PRIO_PROCESS: 按进程 ID 查询/设置
const PRIO_PROCESS: i32 = 0;
/// PRIO_PGRP: 按进程组 ID 查询/设置
const PRIO_PGRP: i32 = 1;
/// PRIO_USER: 按用户 ID 查询/设置
const PRIO_USER: i32 = 2;

/// nice 值的合法范围
const MIN_NICE: i32 = -20;
const MAX_NICE: i32 = 19;

/// 参考 https://man7.org/linux/man-pages/man2/getpriority.2.html
///
/// 返回 `which`/`who` 匹配任务的优先级（20 - nice 值）。
pub fn sys_getpriority(which: i32, who: usize) -> SyscallRet {
    let current = current_task().ok_or(SysErrNo::ESRCH)?;
    let nice = match which {
        PRIO_PROCESS => {
            if who == 0 {
                Some(task_nice(&current))
            } else {
                Process::get_process_arc_by_pid(who).and_then(|process| {
                    // ProcessMeta 只用于取出任务引用；进入 TCB 锁前必须释放它。
                    let task = process
                        .meta_lock()
                        .tasks
                        .iter()
                        .find_map(|task| task.upgrade());
                    task.map(|task| task_nice(&task))
                })
            }
        }
        PRIO_PGRP => {
            let pgid = if who == 0 {
                current.process.pgid()
            } else {
                who
            };
            matching_task_nice(|task| task.process.pgid() == pgid)
        }
        PRIO_USER => {
            let uid = if who == 0 {
                current.inner_lock().user_id
            } else {
                who
            };
            matching_task_nice(|task| task.inner_lock().user_id == uid)
        }
        _ => return Err(SysErrNo::EINVAL),
    };

    match nice {
        Some(nice) => Ok((20 - nice) as usize),
        None => Err(SysErrNo::ESRCH),
    }
}

fn task_nice(task: &TaskControlBlock) -> i32 {
    task.inner_lock().nice
}

/// `getpriority()` returns the numerically lowest nice value among all tasks
/// selected by a process group or real user ID.
fn matching_task_nice(matches: impl Fn(&TaskControlBlock) -> bool) -> Option<i32> {
    let mut lowest_nice: Option<i32> = None;
    tid_to_task::for_each_task(|task| {
        if matches(task) {
            let nice = task_nice(task);
            lowest_nice = Some(lowest_nice.map_or(nice, |lowest| lowest.min(nice)));
        }
    });
    lowest_nice
}

/// 参考 https://man7.org/linux/man-pages/man2/setpriority.2.html
///
/// 设置 `which`/`who` 匹配进程的 nice 值。
/// 目前仅支持 PRIO_PROCESS。
pub fn sys_setpriority(which: i32, who: usize, prio: i32) -> SyscallRet {
    // 参数校验：nice 值必须在 -20..19 范围内
    if prio < MIN_NICE || prio > MAX_NICE {
        return Err(SysErrNo::EINVAL);
    }

    if which != PRIO_PROCESS {
        return Err(SysErrNo::EINVAL);
    }

    // EPERM: 降低优先级（增大 nice 值）需要 CAP_SYS_NICE 或同一用户
    // 这里简化处理：允许所有操作

    if who == 0 {
        let task = current_task().unwrap();
        let mut inner = task.inner_lock();
        inner.nice = prio;
        return Ok(0);
    }

    let proc = Process::get_process_arc_by_pid(who);
    match proc {
        Some(proc) => {
            let meta = proc.meta_lock();
            for task_weak in &meta.tasks {
                if let Some(task) = task_weak.upgrade() {
                    let mut inner = task.inner_lock();
                    inner.nice = prio;
                }
            }
            Ok(0)
        }
        None => Err(SysErrNo::ESRCH),
    }
}

// ---------------------------------------------------------------------------
// getrlimit(163) / setrlimit(164)
// ---------------------------------------------------------------------------

pub const RLIMIT_FSIZE: i32 = 1;
pub const RLIMIT_NOFILE: i32 = 7;
pub const RLIMIT_STACK: i32 = 3;
/// 表示"无限制"的特殊值（与 Linux RLIM64_INFINITY 一致）
pub const RLIM_INFINITY: usize = usize::MAX;

/// 根据资源类型返回默认的 rlimit 值
pub fn default_rlimit(resource: i32) -> RLimit {
    match resource {
        RLIMIT_FSIZE => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
        RLIMIT_NOFILE => RLimit {
            rlim_cur: 128,
            rlim_max: 256,
        },
        RLIMIT_STACK => RLimit {
            rlim_cur: 8 * 1024 * 1024, // 8 MB
            rlim_max: RLIM_INFINITY,
        },
        _ => RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/getrlimit.2.html
///
/// 获取指定资源的软/硬限制。
/// `rlim` 指向用户空间的 `struct rlimit`。
pub fn sys_getrlimit(resource: i32, rlim: usize) -> SyscallRet {
    // NULL 指针检查
    if rlim == 0 || (rlim as isize) < 0 || if_bad_address(rlim) {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    let limit = if resource == RLIMIT_NOFILE {
        let fd_table = &proc_inner.fd_table;
        RLimit {
            rlim_cur: fd_table.get_soft_limit(),
            rlim_max: fd_table.get_hard_limit(),
        }
    } else if resource == RLIMIT_FSIZE {
        proc_inner.get_rlimit_fsize()
    } else {
        default_rlimit(resource)
    };

    copy_to_user(&memory_set, rlim, unsafe {
        core::slice::from_raw_parts(
            &limit as *const RLimit as *const u8,
            core::mem::size_of::<RLimit>(),
        )
    })?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/setrlimit.2.html
///
/// 设置指定资源的软/硬限制。
/// `rlim` 指向用户空间的 `struct rlimit`。
pub fn sys_setrlimit(resource: i32, rlim: usize) -> SyscallRet {
    // NULL 指针检查
    if rlim == 0 || (rlim as isize) < 0 || if_bad_address(rlim) {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    let mut limit = RLimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    copy_from_user(&memory_set, rlim, unsafe {
        core::slice::from_raw_parts_mut(
            &mut limit as *mut RLimit as *mut u8,
            core::mem::size_of::<RLimit>(),
        )
    })?;

    // 软限制不能超过硬限制
    if limit.rlim_cur > limit.rlim_max {
        return Err(SysErrNo::EINVAL);
    }

    if resource == RLIMIT_NOFILE {
        let fd_table = &proc_inner.fd_table;
        fd_table.set_limit(limit.rlim_cur, limit.rlim_max);
    } else if resource == RLIMIT_FSIZE {
        proc_inner.set_rlimit_fsize(limit);
    }
    // 其他资源类型目前静默接受，不实际存储限制

    Ok(0)
}
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
    let inner = &task.process;
    let memory_set = inner.memory_set_arc();
    let fd_table = inner.fd_table.clone();
    if !old_limit.is_null() {
        let limit = if resource as i32 == RLIMIT_NOFILE {
            RLimit {
                rlim_cur: fd_table.get_soft_limit(),
                rlim_max: fd_table.get_hard_limit(),
            }
        } else if resource as i32 == RLIMIT_FSIZE {
            inner.get_rlimit_fsize()
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
        } else if resource as i32 == RLIMIT_FSIZE {
            inner.set_rlimit_fsize(limit);
        }
    }

    Ok(0)
}
