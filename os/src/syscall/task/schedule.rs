use linux_raw_sys::general::{CLOCK_REALTIME, TIMER_ABSTIME};
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

    // 仅支持 CLOCK_REALTIME
    if clockid != CLOCK_REALTIME as usize {
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
    let t = get_data(memory_set.token(), t);
    drop(memory_set);
    drop(process);

    // tv_nsec 必须在 [0, 10^9) 范围内
    if t.tv_nsec >= 1_000_000_000 {
        return Err(SysErrNo::EINVAL);
    }

    // 以纳秒为单位的总睡眠时长
    let total_ns = t.tv_sec * 1_000_000_000 + t.tv_nsec;

    // 绝对时间模式: 计算到目标时刻的剩余纳秒数
    if flags == TIME_ABSTIME {
        let now = get_time_spec();
        // 目标时刻已过 → 立即返回
        if t.tv_sec < now.tv_sec || (t.tv_sec == now.tv_sec && t.tv_nsec <= now.tv_nsec) {
            return Ok(0);
        }
    }

    // 记录起始时间 (纳秒) 和截止时刻 (用于被信号中断时计算剩余)
    let begin_ns = get_time_ms() * 1_000_000;
    let endtime = if flags == TIME_ABSTIME {
        t // 绝对时间: 截止时刻就是 t 本身
    } else {
        get_time_spec() + t // 相对时间: 当前 + 时长
    };

    loop {
        let elapsed_ns = get_time_ms() * 1_000_000 - begin_ns;
        if elapsed_ns >= total_ns {
            break;
        }

        if let Some(_) = check_if_any_sig_for_current_task() {
            if !remain.is_null() {
                let process = task.process.inner_lock();
                let memory_set = process.get_locked_memory_set_read();
                safe_put_data(&*memory_set, remain, calculate_left_timespec(endtime));
            }
            return Err(SysErrNo::EINTR);
        }
        suspend_current_and_run_next();
    }
    Ok(0)
}
