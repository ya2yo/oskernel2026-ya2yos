use crate::mm::{get_data, if_bad_address, put_data};
use crate::task::current_task;
use crate::timer::{
    get_time_ms, get_time_spec, Itimerval, Rusage, TimeVal, Timespec, Tms, ITIMER_REAL,
    NOW_TIME_STAMP,
};
use crate::utils::{SysErrNo, SyscallRet};
use log::{debug, error};

const MAX_CLOCKS: usize = 12;

/// 参考 https://man7.org/linux/man-pages/man2/gettimeofday.2.html
pub fn sys_gettimeofday(ts: *mut Timespec, tz: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let token = task.process.inner_lock().get_locked_memory_set().token();

    if (ts as isize) < 0 || if_bad_address(ts as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if (tz as isize) < 0 || if_bad_address(tz as usize) {
        return Err(SysErrNo::EFAULT);
    }
    let mut time = get_time_spec();
    time.tv_sec += NOW_TIME_STAMP;
    put_data(token, ts, time);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/times.2.html
pub fn sys_times(tms: *mut Tms) -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let token = task.process.inner_lock().get_locked_memory_set().token();

    put_data(token, tms, Tms::new(&task_inner.time_data));
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/setitimer.2.html
pub fn sys_settimer(
    which: usize,
    new_value: *const Itimerval,
    old_value: *mut Itimerval,
) -> SyscallRet {
    // TrustOS目前只支持 ITIMER_REAL
    assert!(which == ITIMER_REAL, "only support Itimer Real");
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let token = task.process.inner_lock().get_locked_memory_set().token();

    if old_value as usize != 0 {
        put_data(token, old_value, task_inner.timer.timer());
    }
    if new_value as usize != 0 {
        let new_timer = get_data(token, new_value);
        debug!("[sys_settimer] new_timer={:?}", new_timer);
        task_inner.timer.set_timer(new_timer);
        task_inner.timer.set_last_time(TimeVal::now());
        if new_timer.it_interval.is_empty() {
            if !new_timer.it_value.is_empty() {
                task_inner.timer.set_trigger_once(true);
            }
        } else {
            task_inner.timer.set_trigger_once(false);
        }
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/clock_gettime.2.html
pub fn sys_clock_gettime(clockid: usize, tp: *mut Timespec) -> SyscallRet {
    // let debug_get_time = get_time();
    // debug!("get_time = {}", debug_get_time);
    if clockid != 0 && clockid != 1 {
        error!("clockid != 0 and 1 ------------ clockid = {}", clockid);
    }
    let task = current_task().unwrap();

    let token = task.process.inner_lock().get_locked_memory_set().token();
    let mut time = get_time_spec();

    if clockid == 1 {
        time = time;
    } else {
        time.tv_sec += NOW_TIME_STAMP;
    }

    if (tp as isize) <= 0 || if_bad_address(tp as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if clockid >= MAX_CLOCKS {
        return Err(SysErrNo::EINVAL);
    }

    put_data(token, tp, time);
    // error!(
    //     "[sys_clock_gettime] clockid is {}, time={:?}",
    //     clockid, time
    // );
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/getrusage.2.html
pub fn sys_getrusage(who: isize, usage: *mut Rusage) -> SyscallRet {
    // TrustOS目前只支持 RUSAGESELF 和 RUSAGECHILDEN
    debug!(
        "[sys_getrusage] who is {}, usage is {:x}",
        who, usage as usize
    );

    if who < -1 {
        return Err(SysErrNo::EINVAL);
    }

    if (usage as isize) < 0 || if_bad_address(usage as usize) {
        return Err(SysErrNo::EFAULT);
    }

    const RUSAGESELF: isize = 0;
    const RUSAGECHILDEN: isize = -1;

    let task = current_task().unwrap();
    let inner = task.inner_lock();
    let token = task.process.inner_lock().get_locked_memory_set().token();

    match who {
        RUSAGESELF => {
            let gotusage = Rusage::new_from_ms(
                inner.time_data.utime as usize,
                inner.time_data.stime as usize,
            );
            put_data(token, usage, gotusage);
            Ok(0)
        }
        RUSAGECHILDEN => {
            let gotusage = Rusage::new_from_ms(
                inner.time_data.cutime as usize,
                inner.time_data.cstime as usize,
            );
            put_data(token, usage, gotusage);
            Ok(0)
        }
        _ => return Err(SysErrNo::EINVAL),
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/clock_getres.2.html
pub fn sys_clock_getres(clockid: usize, res: *mut Timespec) -> SyscallRet {
    debug!(
        "[sys_clock_getres] clockid is {}, res is {:x}",
        clockid, res as usize
    );

    if (clockid as isize) < 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let token = task.process.inner_lock().get_locked_memory_set().token();

    //assert!(clockid == 1, "other clockid not supported!");

    let restime = Timespec::new(0, 1);
    put_data(token, res, restime);
    Ok(0)
}
