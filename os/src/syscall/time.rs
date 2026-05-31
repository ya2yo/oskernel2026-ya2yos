use crate::mm::{get_data, if_bad_address, put_data};
use crate::task::current_task;
use crate::timer::{
    CLOCK_REALTIME_OFFSET, ITIMER_REAL, Itimerval, NANOS_PER_SEC, NOW_TIME_STAMP, Rusage, TIME_OK, TimeVal, Timespec, Timex, Tms, get_time_ms, get_time_spec, timex_apply, timex_get_realtime
};
use crate::utils::{SysErrNo, SyscallRet};
use linux_raw_sys::general::CLOCK_REALTIME;
use log::{debug, error};

const MAX_CLOCKS: usize = 12;

/// 参考 https://man7.org/linux/man-pages/man2/gettimeofday.2.html
pub fn sys_gettimeofday(ts: *mut Timespec, tz: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();

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
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();

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
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();

    if old_value as usize != 0 {
        put_data(token, old_value, task_inner.timer.timer());
    }
    if new_value as usize != 0 {
        let new_timer = get_data(token, new_value);
        // debug!("[sys_settimer] new_timer={:?}", new_timer);
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
    // if clockid != 0 && clockid != 1 {
    //     error!("clockid != 0 and 1 ------------ clockid = {}", clockid);
    // }
    let task = current_task().unwrap();

    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();
    let mut time = get_time_spec();

    if clockid == 1 {
        // monotonic: do nothing
    } else {
        time.tv_sec += NOW_TIME_STAMP;
        time.tv_sec = (time.tv_sec as i64 + *CLOCK_REALTIME_OFFSET.lock()) as usize;
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
    // debug!(
    //     "[sys_getrusage] who is {}, usage is {:x}",
    //     who, usage as usize
    // );

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
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();

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
    // debug!(
    //     "[sys_clock_getres] clockid is {}, res is {:x}",
    //     clockid, res as usize
    // );

    if (clockid as isize) < 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();

    //assert!(clockid == 1, "other clockid not supported!");

    let restime = Timespec::new(0, 1);
    put_data(token, res, restime);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/adjtimex.2.html
///
/// 读取/设置内核时钟同步参数。
/// - modes == 0: 将当前时钟参数写入 *buf，返回 TIME_OK (0)
/// - modes != 0: 应用 buf 中的设置，返回 TIME_OK (0)
pub fn sys_adjtimex(buf: *mut Timex) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let token = proc_inner.get_locked_memory_set_read().token();
    let privileged = task.inner_lock().effective_uid == 0;

    if (buf as isize) <= 0 || if_bad_address(buf as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let tx = get_data(token, buf);

    if tx.modes == 0 {
        put_data(token, buf, timex_get_realtime());
        debug!("[adjtimex] read current params, returning TIME_OK");
        return Ok(TIME_OK);
    }

    debug!(
        "[adjtimex] set params, modes=0x{:x}, offset={}, freq={}, status={}",
        tx.modes, tx.offset, tx.freq, tx.status
    );

    let ret = timex_apply(&tx, privileged)?;
    put_data(token, buf, timex_get_realtime());
    Ok(ret)
}

/// 参考 https://man7.org/linux/man-pages/man2/clock_adjtime.2.html
///
/// 仅 CLOCK_REALTIME 支持 adjtime；语义与 adjtimex 相同。
pub fn sys_clock_adjtime(clock_id: u32, buf: *mut Timex) -> SyscallRet {
    if clock_id != CLOCK_REALTIME {
        return Err(SysErrNo::EINVAL);
    }

    if (buf as isize) <= 0 || if_bad_address(buf as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let token = proc_inner.get_locked_memory_set_read().token();
    let privileged = task.inner_lock().effective_uid == 0;

    let tx = get_data(token, buf);

    if tx.modes == 0 {
        put_data(token, buf, timex_get_realtime());
        debug!("[clock_adjtime] read current params");
        return Ok(TIME_OK);
    }

    debug!(
        "[clock_adjtime] clk_id={}, modes=0x{:x}, offset={}, freq={}",
        clock_id, tx.modes, tx.offset, tx.freq
    );

    let ret = timex_apply(&tx, privileged)?;
    put_data(token, buf, timex_get_realtime());
    Ok(ret)
}

// ---------------------------------------------------------------------------
// clock_settime(112)
// ---------------------------------------------------------------------------

/// 参考 https://man7.org/linux/man-pages/man2/clock_settime.2.html
pub fn sys_clock_settime(clock_id: u32, tp: *const Timespec) -> SyscallRet {
    // 仅 CLOCK_REALTIME 可设置
    if clock_id != CLOCK_REALTIME as u32 {
        return Err(SysErrNo::EINVAL);
    }

    // EFAULT: 空指针或非法地址
    if tp.is_null() || (tp as isize) <= 0 || if_bad_address(tp as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();
    let ts = get_data(token, tp);

    // EINVAL: tv_nsec < 0 或 >= 10^9
    if  ts.tv_nsec as u64 >= NANOS_PER_SEC {
        return Err(SysErrNo::EINVAL);
    }

    // 计算偏移量: offset = desired_sec - current_raw_sec
    let desired_sec = ts.tv_sec as i64;
    let current_raw_sec = (get_time_spec().tv_sec + NOW_TIME_STAMP) as i64;
    *CLOCK_REALTIME_OFFSET.lock() = desired_sec - current_raw_sec;

    debug!(
        "[clock_settime] set CLOCK_REALTIME to {}.{:09}, offset={}",
        ts.tv_sec, ts.tv_nsec, desired_sec - current_raw_sec
    );
    Ok(0)
}
