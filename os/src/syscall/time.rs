use crate::mm::{copy_from_user, copy_to_user, if_bad_address};
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
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    if (ts as isize) < 0 || if_bad_address(ts as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if (tz as isize) < 0 || if_bad_address(tz as usize) {
        return Err(SysErrNo::EFAULT);
    }
    let mut time = get_time_spec();
    time.tv_sec += NOW_TIME_STAMP;
    copy_to_user(&memory_set, ts as usize, unsafe {
        core::slice::from_raw_parts(&time as *const Timespec as *const u8, core::mem::size_of::<Timespec>())
    })?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/times.2.html
pub fn sys_times(tms: *mut Tms) -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let tms_data = Tms::new(&task_inner.time_data);
    copy_to_user(&memory_set, tms as usize, unsafe {
        core::slice::from_raw_parts(&tms_data as *const Tms as *const u8, core::mem::size_of::<Tms>())
    })?;
    Ok(0)
}

/// 参考 https://www.man7.org/linux/man-pages/man2/setitimer.2.html
///
/// 读取指定 itimer 的当前值到用户空间的 `struct itimerval`。
/// 仿照 sys_settimer 的模式：读 task_inner.timer.timer() → copy_to_user。
pub fn sys_gettimer(_which: i32, curr_value: usize) -> SyscallRet {
    if curr_value == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    let timer = task_inner.timer.timer();
    copy_to_user(&memory_set, curr_value, unsafe {
        core::slice::from_raw_parts(
            &timer as *const Itimerval as *const u8,
            core::mem::size_of::<Itimerval>(),
        )
    })?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/setitimer.2.html
pub fn sys_settimer(
    _which: usize,
    new_value: *const Itimerval,
    old_value: *mut Itimerval,
) -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    if old_value as usize != 0 {
        let timer = task_inner.timer.timer();
        copy_to_user(&memory_set, old_value as usize, unsafe{
            core::slice::from_raw_parts(&timer as *const Itimerval as *const _, 
            core::mem::size_of::<Itimerval>())
        })?;
    }
    if new_value as usize != 0 {
        let mut new_timer = Itimerval::default();
        copy_from_user(&memory_set, new_value as usize, unsafe{
            core::slice::from_raw_parts_mut(&mut new_timer as *mut Itimerval as *mut _, 
            core::mem::size_of::<Itimerval>())
        })?;
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

    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
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

    copy_to_user(&memory_set, tp as usize, unsafe {
        core::slice::from_raw_parts(&time as *const Timespec as *const u8, core::mem::size_of::<Timespec>())
    })?;
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
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    match who {
        RUSAGESELF => {
            let gotusage = Rusage::new_from_ms(
                inner.time_data.utime as usize,
                inner.time_data.stime as usize,
            );
            copy_to_user(&memory_set, usage as usize, unsafe {
                core::slice::from_raw_parts(&gotusage as *const Rusage as *const u8, core::mem::size_of::<Rusage>())
            })?;
            Ok(0)
        }
        RUSAGECHILDEN => {
            let gotusage = Rusage::new_from_ms(
                inner.time_data.cutime as usize,
                inner.time_data.cstime as usize,
            );
            copy_to_user(&memory_set, usage as usize, unsafe {
                core::slice::from_raw_parts(&gotusage as *const Rusage as *const u8, core::mem::size_of::<Rusage>())
            })?;
            Ok(0)
        }
        _ => return Err(SysErrNo::EINVAL),
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/clock_getres.2.html
pub fn sys_clock_getres(clockid: usize, res: usize) -> SyscallRet {
    // debug!(
    //     "[sys_clock_getres] clockid is {}, res is {:x}",
    //     clockid, res as usize
    // );

    if (clockid as isize) < 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    //assert!(clockid == 1, "other clockid not supported!");
    let restime = Timespec::new(0, 1);
    copy_to_user(
        &memory_set, 
        res, 
        unsafe { 
            core::slice::from_raw_parts(&restime as *const Timespec as *const _ , 
            core::mem::size_of::<Timespec>()) 
        }
    )?;

    Ok(0) // 返回成功
}

/// 参考 https://man7.org/linux/man-pages/man2/adjtimex.2.html
///
/// 读取/设置内核时钟同步参数。
/// - modes == 0: 将当前时钟参数写入 *buf，返回 TIME_OK (0)
/// - modes != 0: 应用 buf 中的设置，返回 TIME_OK (0)
pub fn sys_adjtimex(buf: *mut Timex) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let privileged = task.inner_lock().effective_uid == 0;

    if (buf as isize) <= 0 || if_bad_address(buf as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let mut tx = Timex::defaults();
    copy_from_user(&memory_set, buf as usize, unsafe {
        core::slice::from_raw_parts_mut(&mut tx as *mut Timex as *mut u8, core::mem::size_of::<Timex>())
    })?;

    if tx.modes == 0 {
        let realtime = timex_get_realtime();
        copy_to_user(&memory_set, buf as usize, unsafe {
            core::slice::from_raw_parts(&realtime as *const Timex as *const u8, core::mem::size_of::<Timex>())
        })?;
        debug!("[adjtimex] read current params, returning TIME_OK");
        return Ok(TIME_OK);
    }

    debug!(
        "[adjtimex] set params, modes=0x{:x}, offset={}, freq={}, status={}",
        tx.modes, tx.offset, tx.freq, tx.status
    );

    let ret = timex_apply(&tx, privileged)?;
    let realtime = timex_get_realtime();
    copy_to_user(&memory_set, buf as usize, unsafe {
        core::slice::from_raw_parts(&realtime as *const Timex as *const u8, core::mem::size_of::<Timex>())
    })?;
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
    let memory_set = proc_inner.get_locked_memory_set_read();
    let privileged = task.inner_lock().effective_uid == 0;

    let mut tx = Timex::defaults();
    copy_from_user(&memory_set, buf as usize, unsafe {
        core::slice::from_raw_parts_mut(&mut tx as *mut Timex as *mut u8, core::mem::size_of::<Timex>())
    })?;

    if tx.modes == 0 {
        let realtime = timex_get_realtime();
        copy_to_user(&memory_set, buf as usize, unsafe {
            core::slice::from_raw_parts(&realtime as *const Timex as *const u8, core::mem::size_of::<Timex>())
        })?;
        debug!("[clock_adjtime] read current params");
        return Ok(TIME_OK);
    }

    debug!(
        "[clock_adjtime] clk_id={}, modes=0x{:x}, offset={}, freq={}",
        clock_id, tx.modes, tx.offset, tx.freq
    );

    let ret = timex_apply(&tx, privileged)?;
    let realtime = timex_get_realtime();
    copy_to_user(&memory_set, buf as usize, unsafe {
        core::slice::from_raw_parts(&realtime as *const Timex as *const u8, core::mem::size_of::<Timex>())
    })?;
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
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let mut ts = Timespec::new(0, 0);
    copy_from_user(&memory_set, tp as usize, unsafe {
        core::slice::from_raw_parts_mut(&mut ts as *mut Timespec as *mut u8, core::mem::size_of::<Timespec>())
    })?;

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

// ---------------------------------------------------------------------------
// settimeofday(170)
// ---------------------------------------------------------------------------

/// 参考 https://man7.org/linux/man-pages/man2/settimeofday.2.html
///
/// 设置系统实时时钟 (CLOCK_REALTIME)。
/// - `tv`: 要设置的时间值 (struct timeval)
/// - `tz`: 时区信息 (已废弃，应为 NULL，否则返回 EINVAL)
pub fn sys_settimeofday(tv: *const TimeVal, tz: *const u8) -> SyscallRet {
    // EFAULT: tv 不能为空
    if tv.is_null() || (tv as isize) <= 0 || if_bad_address(tv as usize) {
        return Err(SysErrNo::EFAULT);
    }

    // tz 参数已废弃，通常应为 NULL
    if !tz.is_null() {
        return Err(SysErrNo::EINVAL);
    }

    // EPERM: 只有 root 可以设置系统时间
    let task = current_task().unwrap();
    if task.inner_lock().effective_uid != 0 {
        return Err(SysErrNo::EPERM);
    }

    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    let mut timeval = TimeVal::new(0, 0);
    copy_from_user(&memory_set, tv as usize, unsafe {
        core::slice::from_raw_parts_mut(
            &mut timeval as *mut TimeVal as *mut u8,
            core::mem::size_of::<TimeVal>(),
        )
    })?;

    // EINVAL: tv_usec 必须在 [0, 10^6) 范围内
    if timeval.tv_usec >= 1_000_000 {
        return Err(SysErrNo::EINVAL);
    }

    // 转换为 Timespec 并设置 CLOCK_REALTIME
    let desired_sec = timeval.tv_sec as i64;
    let current_raw_sec = (get_time_spec().tv_sec + NOW_TIME_STAMP) as i64;
    *CLOCK_REALTIME_OFFSET.lock() = desired_sec - current_raw_sec;

    debug!(
        "[settimeofday] set time to {}.{:06}, offset={}",
        timeval.tv_sec, timeval.tv_usec, desired_sec - current_raw_sec
    );
    Ok(0)
}
