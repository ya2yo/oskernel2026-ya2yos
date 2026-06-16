use core::sync::atomic::AtomicU32;

use crate::{
    arch::time::get_clock_freq,
    mm::{copy_from_user, copy_to_user, if_bad_address},
    signal::check_if_any_sig_for_current_task,
    task::{current_task, suspend_current_and_run_next, tid_to_task, Process},
    timer::{
        calculate_left_timespec, get_time_ms, get_time_spec, Timespec, MSEC_PER_SEC, NANOS_PER_SEC,
    },
    utils::{SysErrNo, SyscallRet},
};
use linux_raw_sys::general::{
    CLOCK_MONOTONIC, CLOCK_PROCESS_CPUTIME_ID, CLOCK_REALTIME, CLOCK_THREAD_CPUTIME_ID,
    TIMER_ABSTIME,
};
use log::debug;

/// 参考 https://man7.org/linux/man-pages/man2/sched_yield.2.html
pub fn sys_sched_yield() -> SyscallRet {
    suspend_current_and_run_next();
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/nanosleep.2.html
pub fn sys_nanosleep(req: *const Timespec, rem: *mut Timespec) -> SyscallRet {
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut req_val = Timespec::new(0, 0);
    copy_from_user(&memory_set, req as usize, unsafe {
        core::slice::from_raw_parts_mut(
            &mut req_val as *mut Timespec as *mut u8,
            core::mem::size_of::<Timespec>(),
        )
    })?;
    drop(memory_set);
    drop(process);
    let req = req_val;

    if req.tv_nsec >= NANOS_PER_SEC as usize || (req.tv_sec as isize) < 0 {
        return Err(SysErrNo::EINVAL);
    }

    let waittime = req.tv_sec * (NANOS_PER_SEC as usize) + req.tv_nsec;
    let begin = get_time_ms() * (NANOS_PER_SEC as usize / MSEC_PER_SEC);
    let endtime = get_time_spec() + req;

    debug!(
        "[sys_nanosleep] ready to sleep for {} sec, {} nsec",
        req.tv_sec, req.tv_nsec
    );

    while get_time_ms() * 1_000_000usize - begin < waittime {
        if check_if_any_sig_for_current_task().is_some() || {
            let mut task_inner = task.inner_lock();
            let eintr = task_inner.sig_eintr;
            if eintr {
                task_inner.sig_eintr = false;
            }
            eintr
        } {
            if rem as usize != 0 {
                let process = task.process.inner_lock();
                let memory_set = process.get_locked_memory_set_read();
                let left = calculate_left_timespec(endtime);
                copy_to_user(&memory_set, rem as usize, unsafe {
                    core::slice::from_raw_parts(
                        &left as *const Timespec as *const u8,
                        core::mem::size_of::<Timespec>(),
                    )
                })?;
            }
            return Err(SysErrNo::EINTR);
        }
        suspend_current_and_run_next();
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_setaffinity.2.html
pub fn sys_sched_setaffinity(pid: usize, cpusetsize: usize, mask: usize) -> SyscallRet {
    let mask_bytes = core::mem::size_of::<usize>();
    if cpusetsize < mask_bytes {
        return Err(SysErrNo::EINVAL);
    }
    if mask == 0 || if_bad_address(mask) {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    if pid != 0
        && Process::get_process_arc_by_pid(pid).is_none()
        && tid_to_task::tid2task(pid).is_none()
    {
        return Err(SysErrNo::ESRCH);
    }

    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut raw_mask = [0u8; core::mem::size_of::<usize>()];
    copy_from_user(&memory_set, mask, &mut raw_mask)?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_getaffinity.2.html
pub fn sys_sched_getaffinity(pid: usize, cpusetsize: usize, mask: usize) -> SyscallRet {
    let mask_bytes = core::mem::size_of::<usize>();
    if cpusetsize < mask_bytes {
        return Err(SysErrNo::EINVAL);
    }
    if mask == 0 || if_bad_address(mask) {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    if pid != 0
        && Process::get_process_arc_by_pid(pid).is_none()
        && tid_to_task::tid2task(pid).is_none()
    {
        return Err(SysErrNo::ESRCH);
    }

    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let cpu0_mask = 1usize.to_ne_bytes();
    copy_to_user(&memory_set, mask, &cpu0_mask)?;
    Ok(mask_bytes)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_setscheduler.2.html
pub fn sys_sched_setscheduler(_pid: usize, _policy: usize, _param: *const u8) -> SyscallRet {
    // debug!(
    //     "[sys_sched_setscheduler] pid is {}, policy is {}, param is {:x}",
    //     pid, policy, param as usize
    // );
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_getscheduler.2.html
pub fn sys_sched_getscheduler(_pid: usize) -> SyscallRet {
    // debug!("[sys_sched_getscheduler] pid is {}", pid);
    //由于使用的是标准的时间片调度算法，直接返回SCHED_OHTER = 0
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_getparam.2.html
pub fn sys_sched_getparam(pid: usize, param: *mut u8) -> SyscallRet {
    // debug!(
    //     "[sys_sched_getparam] pid is {}, param is {:x}",
    //     pid, param as usize
    // );
    if param.is_null() || if_bad_address(param as usize) {
        return Err(SysErrNo::EFAULT);
    }
    if pid != 0
        && Process::get_process_arc_by_pid(pid).is_none()
        && tid_to_task::tid2task(pid).is_none()
    {
        return Err(SysErrNo::ESRCH);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let sched_priority = 0i32.to_ne_bytes();
    copy_to_user(&memory_set, param as usize, &sched_priority)?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/clock_nanosleep.2.html
pub fn sys_clock_nanosleep(
    clockid: usize,
    flags: u32,
    t: *const Timespec,
    remain: *mut Timespec,
) -> SyscallRet {
    // Linux 仅支持 CLOCK_REALTIME / CLOCK_MONOTONIC
    // CLOCK_PROCESS_CPUTIME_ID / CLOCK_THREAD_CPUTIME_ID → EOPNOTSUPP
    // 其他 clockid → EINVAL
    if clockid == CLOCK_PROCESS_CPUTIME_ID as usize || clockid == CLOCK_THREAD_CPUTIME_ID as usize {
        return Err(SysErrNo::EOPNOTSUPP);
    }
    if clockid != CLOCK_REALTIME as usize && clockid != CLOCK_MONOTONIC as usize {
        return Err(SysErrNo::EINVAL);
    }
    // t 必须有效
    if t.is_null() || (t as isize) <= 0 || if_bad_address(t as usize) {
        return Err(SysErrNo::EFAULT);
    }
    // remain 可为 NULL (调用者不关心剩余时间)，非 NULL 则必须有效
    if !remain.is_null() && ((remain as isize) <= 0 || if_bad_address(remain as usize)) {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut t_val = Timespec::new(0, 0);
    copy_from_user(&memory_set, t as usize, unsafe {
        core::slice::from_raw_parts_mut(
            &mut t_val as *mut Timespec as *mut u8,
            core::mem::size_of::<Timespec>(),
        )
    })?;
    let t = t_val;
    debug!(
        "[sys_clock_nanosleep] clock_id={clockid}, flags={flags}, t={:?}",
        t
    );
    drop(memory_set);
    drop(process);
    // tv_nsec 必须在 [0, 10^9) 范围内
    if t.tv_nsec >= 1_000_000_000 {
        return Err(SysErrNo::EINVAL);
    }
    // 截止时刻 (Timespec, 用于被信号中断时计算剩余)
    let endtime = if flags == TIMER_ABSTIME {
        t // 绝对时间: 截止时刻就是 t 本身
    } else {
        get_time_spec() + t // 相对时间: 当前 + 时长
    };
    // 以微秒为单位的总睡眠时长 (与 get_time_ms() 单位一致)
    let total_us = if flags == TIMER_ABSTIME {
        // 绝对时间: CLOCK_REALTIME 用墙上时钟, CLOCK_MONOTONIC 用开机时间
        let now = if clockid == CLOCK_REALTIME as usize {
            crate::timer::wall_time()
        } else {
            get_time_spec()
        };
        if t.tv_sec < now.tv_sec || (t.tv_sec == now.tv_sec && t.tv_nsec <= now.tv_nsec) {
            return Ok(0); // 目标已过
        }
        let diff_sec = t.tv_sec - now.tv_sec;
        let diff_nsec = t.tv_nsec as isize - now.tv_nsec as isize;
        if diff_nsec < 0 {
            (diff_sec as u64 - 1) * 1_000_000 + ((1_000_000_000isize + diff_nsec) / 1000) as u64
        } else {
            diff_sec as u64 * 1_000_000 + (diff_nsec as u64 / 1000)
        }
    } else {
        // 相对时间: tv_sec*10^6 + tv_nsec/10^3 (微秒)
        t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1000
    };
    // 记录起始时间 (毫秒)
    let begin_ticks = crate::arch::time::get_ticks();

    loop {
        let now_ticks = crate::arch::time::get_ticks();
        let elapsed_ticks = now_ticks - begin_ticks;

        // 计算经过的微秒数
        let elapsed_us = (elapsed_ticks * 1_000_000) / (get_clock_freq() / 1000);
        if elapsed_us >= total_us as usize {
            break;
        }

        // 检查信号：pending 信号或已被 trap handler 拦截的信号
        if check_if_any_sig_for_current_task().is_some() || {
            let mut task_inner = task.inner_lock();
            let eintr = task_inner.sig_eintr;
            if eintr {
                task_inner.sig_eintr = false;
            }
            eintr
        } {
            if !remain.is_null() {
                let process = task.process.inner_lock();
                let memory_set = process.get_locked_memory_set_read();
                let left = calculate_left_timespec(endtime);
                copy_to_user(&memory_set, remain as usize, unsafe {
                    core::slice::from_raw_parts(
                        &left as *const Timespec as *const u8,
                        core::mem::size_of::<Timespec>(),
                    )
                })?;
            }
            return Err(SysErrNo::EINTR);
        }
        suspend_current_and_run_next();
    }
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/sched_get_priority_max.2.html
///
/// 返回指定调度策略的最大静态优先级。
/// 策略: 0=SCHED_OTHER, 1=SCHED_FIFO, 2=SCHED_RR
pub fn sys_sched_get_priority_max(policy: i32) -> SyscallRet {
    match policy {
        0 => Ok(0),  // SCHED_OTHER: 仅一个优先级
        1 => Ok(99), // SCHED_FIFO
        2 => Ok(99), // SCHED_RR
        _ => Err(SysErrNo::EINVAL),
    }
}
/// https://man7.org/linux/man-pages/man2/sched_get_priority_min.2.html
///
/// 返回指定调度策略的最小静态优先级。
pub fn sys_sched_get_priority_min(policy: i32) -> SyscallRet {
    match policy {
        0 => Ok(0), // SCHED_OTHER: 仅一个优先级
        1 => Ok(1), // SCHED_FIFO
        2 => Ok(1), // SCHED_RR
        _ => Err(SysErrNo::EINVAL),
    }
}
