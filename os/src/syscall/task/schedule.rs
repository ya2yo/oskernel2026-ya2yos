use log::debug;

use crate::{
    mm::{get_data, if_bad_address, put_data, safe_put_data},
    signal::check_if_any_sig_for_current_task,
    task::{current_task, current_token, suspend_current_and_run_next},
    timer::{calculate_left_timespec, get_time_ms, get_time_spec, Timespec},
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/sched_yield.2.html
pub fn sys_sched_yield() -> SyscallRet {
    suspend_current_and_run_next();
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/nanosleep.2.html
pub fn sys_nanosleep(req: *const Timespec, rem: *mut Timespec) -> SyscallRet {
    let token = current_token();

    // debug!(
    //     "[sys_nanosleep] req is {:x}, rem is {:x}",
    //     req as usize, rem as usize
    // );

    let req = get_data(token, req);
    let waittime = req.tv_sec * 1_000_000_000usize + req.tv_nsec;
    let begin = get_time_ms() * 1_000_000usize;
    let endtime = get_time_spec() + req;

    // debug!(
    //     "[sys_nanosleep] ready to sleep for {} sec, {} nsec",
    //     req.tv_sec, req.tv_nsec
    // );

    while get_time_ms() * 1_000_000usize - begin < waittime {
        if let Some(_) = check_if_any_sig_for_current_task() {
            //被信号唤醒
            if rem as usize != 0 {
                put_data(token, rem, calculate_left_timespec(endtime));
            }
            return Err(SysErrNo::EINTR);
        }
        suspend_current_and_run_next();
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_setaffinity.2.html
pub fn sys_sched_setaffinity(_pid: usize, _cpusetsize: usize, _mask: usize) -> SyscallRet {
    // debug!(
    //     "[sys_sched_setaffinity] pid is {}, cpusetsize is {}, mask is {}",
    //     pid, cpusetsize, mask
    // );
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_getaffinity.2.html
pub fn sys_sched_getaffinity(_pid: usize, _cpusetsize: usize, _mask: usize) -> SyscallRet {
    // debug!(
    //     "[sys_sched_getaffinity] pid is {}, cpusetsize is {}, mask is {}",
    //     pid, cpusetsize, mask
    // );
    Ok(0)
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
pub fn sys_sched_getparam(_pid: usize, _param: *const u8) -> SyscallRet {
    // debug!(
    //     "[sys_sched_getparam] pid is {}, param is {:x}",
    //     pid, param as usize
    // );
    //由于使用的是标准的时间片调度算法，param参数需要被忽略
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/clock_nanosleep.2.html
pub fn sys_clock_nanosleep(
    clockid: usize,
    flags: u32,
    t: *const Timespec,
    remain: *mut Timespec,
) -> SyscallRet {
    const TIME_ABSTIME: u32 = 1;
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();

    if clockid != 0 {
        return Err(SysErrNo::EOPNOTSUPP);
    }

    if (t as isize) <= 0 || if_bad_address(t as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if (remain as isize) < 0 || if_bad_address(remain as usize) {
        return Err(SysErrNo::EFAULT);
    }

    debug!(
        "[sys_clock_nanosleep] clockid is {}, flags is {}, t is {:x}, remain is {:x}",
        clockid, flags, t as usize, remain as usize
    );

    let t = get_data(memory_set.token(), t);
    drop(memory_set);
    drop(process);
    if t.tv_nsec >= 1_000_000_000usize {
        return Err(SysErrNo::EINVAL);
    }

    let waittime = t.tv_sec * 1_000_000_000usize + t.tv_nsec;

    let begin = get_time_ms() * 1_000_000usize;
    let endtime = if flags == TIME_ABSTIME {
        //绝对时间
        t
    } else {
        //相对时间
        get_time_spec() + t
    };

    debug!(
        "[sys_clock_nanosleep] ready to sleep for {} sec, {} nsec",
        t.tv_sec, t.tv_nsec
    );

    while get_time_ms() * 1_000_000usize - begin < waittime {
        if let Some(_) = check_if_any_sig_for_current_task() {
            //被信号唤醒
            debug!("interupt by signal");
            if remain as usize != 0 {
                let process = task.process.inner_lock();
                let memory_set = process.get_locked_memory_set_read();
                safe_put_data(&*memory_set, remain, calculate_left_timespec(endtime));
            }
            //handle_signal(signo);
            return Err(SysErrNo::EINTR);
        }
        suspend_current_and_run_next();
    }
    Ok(0)
}
