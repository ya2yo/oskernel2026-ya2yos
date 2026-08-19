use core::mem::size_of;

use alloc::sync::Arc;
use linux_raw_sys::{
    general::CAP_SYS_ADMIN,
    prctl::{
        PR_CAPBSET_DROP, PR_CAPBSET_READ, PR_CAP_AMBIENT, PR_GET_CHILD_SUBREAPER, PR_GET_DUMPABLE,
        PR_GET_NO_NEW_PRIVS, PR_GET_PDEATHSIG, PR_GET_SECCOMP, PR_GET_SPECULATION_CTRL,
        PR_GET_THP_DISABLE, PR_GET_TIMERSLACK, PR_MCE_KILL, PR_MCE_KILL_CLEAR, PR_MCE_KILL_DEFAULT,
        PR_MCE_KILL_EARLY, PR_MCE_KILL_GET, PR_MCE_KILL_LATE, PR_MCE_KILL_SET,
        PR_SET_CHILD_SUBREAPER, PR_SET_DUMPABLE, PR_SET_NAME, PR_SET_NO_NEW_PRIVS,
        PR_SET_PDEATHSIG, PR_SET_SECCOMP, PR_SET_SECUREBITS, PR_SET_THP_DISABLE, PR_SET_TIMERSLACK,
        PR_SET_TIMING,
    },
};
use log::{debug, warn};

use crate::{
    mm::{copy_from_user, copy_from_user_val, copy_to_user, copy_to_user_val, if_bad_address},
    task::{current_task, SeccompState, SockFilter, SECCOMP_FILTER_MAX_INSNS},
    utils::{SysErrNo, SyscallRet},
};

const SECCOMP_MODE_STRICT: u32 = 1;
const SECCOMP_MODE_FILTER: u32 = 2;
const SECCOMP_SET_MODE_STRICT: u32 = 0;
const SECCOMP_SET_MODE_FILTER: u32 = 1;
const SECCOMP_SET_MODE_STRICT_FAIL: u32 = 0x100;
const SECCOMP_SET_MODE_WHITELIST: u32 = 0x101;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct seccomp_whitelist {
    len: u16,
    syscalls: *const i32,
}

/// https://man7.org/linux/man-pages/man2/seccomp.2.html
/// `seccomp(operation, flags, uargs)` — install a seccomp policy directly.
pub fn sys_seccomp(operation: u32, flags: u32, uargs: usize) -> SyscallRet {
    if flags != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    match operation {
        SECCOMP_SET_MODE_STRICT => {
            if uargs != 0 {
                return Err(SysErrNo::EINVAL);
            }
            let mut inner = task.inner_lock();
            if !inner.seccomp_state.is_disabled() {
                return Err(SysErrNo::EINVAL);
            }
            inner.seccomp_state = SeccompState::Strict;
            Ok(0)
        }
        SECCOMP_SET_MODE_FILTER => {
            if !task.inner_lock().seccomp_state.is_disabled() {
                return Err(SysErrNo::EINVAL);
            }
            let no_new_privs = task.inner_lock().no_new_privs;
            if !no_new_privs && !current_has_cap_sys_admin() {
                return Err(SysErrNo::EACCES);
            }
            if uargs == 0 || if_bad_address(uargs) {
                return Err(SysErrNo::EFAULT);
            }

            let memory_set = task.process.memory_set_arc();
            let fprog = copy_from_user_val(&memory_set, uargs as *const SockFprog)?;
            let filter_len = fprog.len as usize;
            if filter_len == 0 || filter_len > SECCOMP_FILTER_MAX_INSNS {
                return Err(SysErrNo::EINVAL);
            }
            if fprog.filter.is_null() || if_bad_address(fprog.filter as usize) {
                return Err(SysErrNo::EFAULT);
            }

            let mut filter = alloc::vec![SockFilter::default(); filter_len];
            let filter_bytes = unsafe {
                core::slice::from_raw_parts_mut(
                    filter.as_mut_ptr() as *mut u8,
                    filter_len * size_of::<SockFilter>(),
                )
            };
            copy_from_user(&memory_set, fprog.filter as usize, filter_bytes)?;
            let seccomp_state = SeccompState::new_filter(filter).ok_or(SysErrNo::EINVAL)?;

            let mut inner = task.inner_lock();
            if !inner.seccomp_state.is_disabled() {
                return Err(SysErrNo::EINVAL);
            }
            inner.seccomp_state = seccomp_state;
            Ok(0)
        }
        SECCOMP_SET_MODE_STRICT_FAIL => {
            if uargs != 0 {
                return Err(SysErrNo::EINVAL);
            }
            let mut inner = task.inner_lock();
            if !inner.seccomp_state.is_disabled() {
                return Err(SysErrNo::EINVAL);
            }
            inner.seccomp_state = SeccompState::StrictFail;
            Ok(0)
        }
        SECCOMP_SET_MODE_WHITELIST => {
            if !task.inner_lock().seccomp_state.is_disabled() {
                return Err(SysErrNo::EINVAL);
            }
            let no_new_privs = task.inner_lock().no_new_privs;
            if !no_new_privs && !current_has_cap_sys_admin() {
                return Err(SysErrNo::EACCES);
            }
            if uargs == 0 || if_bad_address(uargs) {
                return Err(SysErrNo::EFAULT);
            }
            let memory_set = task.process.memory_set_arc();

            let seccomp_whitelist =
                copy_from_user_val(&memory_set, uargs as *const seccomp_whitelist)?;
            let white_len = seccomp_whitelist.len as usize;
            if seccomp_whitelist.syscalls.is_null()
                || if_bad_address(seccomp_whitelist.syscalls as usize)
            {
                return Err(SysErrNo::EFAULT);
            }
            let mut white_lists = alloc::vec![0u32; white_len];
            let white_lists_bytes = unsafe {
                core::slice::from_raw_parts_mut(
                    white_lists.as_mut_ptr() as *mut u8,
                    white_len * size_of::<u32>(),
                )
            };
            copy_from_user(&memory_set, seccomp_whitelist.syscalls as usize, white_lists_bytes)?;
            let seccomp_state = SeccompState::new_white_list(white_lists).ok_or(SysErrNo::EINVAL)?;
            let mut inner = task.inner_lock();
            if !inner.seccomp_state.is_disabled() {
                return Err(SysErrNo::EINVAL);
            }
            inner.seccomp_state = seccomp_state;
            Ok(0)
        }
        _ => Err(SysErrNo::EINVAL),
    }
}

fn current_has_cap_sys_admin() -> bool {
    let task = current_task().unwrap();
    let inner = task.inner_lock();
    let word = (CAP_SYS_ADMIN / 32) as usize;
    let bit = 1u32 << (CAP_SYS_ADMIN % 32);
    inner
        .capabilities
        .effective
        .get(word)
        .map_or(false, |capabilities| capabilities & bit != 0)
}

// prctl(2) — 进程控制
/// 参考 https://man7.org/linux/man-pages/man2/prctl.2.html
pub fn sys_prctl(option: u32, arg2: u32, arg3: usize, arg4: usize, arg5: usize) -> SyscallRet {
    debug!(
        "[prctl] option={}, arg2=0x{:x}, arg3=0x{:x}, arg4=0x{:x}, arg5=0x{:x}",
        option, arg2, arg3, arg4, arg5
    );
    let task = current_task().unwrap();
    match option {
        PR_SET_PDEATHSIG => {
            // arg2: 信号编号，0 表示清除
            if arg2 > 64 {
                return Err(SysErrNo::EINVAL);
            }
            let mut inner = task.inner_lock();
            inner.pdeath_signal = arg2 as u8;
            debug!("[prctl] set pdeath_signal={}", arg2);
            Ok(0)
        }
        PR_GET_PDEATHSIG => {
            // 将当前 pdeath_signal 写入 arg2 指向的 int
            let inner = task.inner_lock();
            let sig = inner.pdeath_signal;
            drop(inner);
            if arg2 != 0 {
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
                let sig_val = sig as i32;
                copy_to_user(&memory_set, arg2 as usize, unsafe {
                    core::slice::from_raw_parts(
                        &sig_val as *const i32 as *const u8,
                        core::mem::size_of::<i32>(),
                    )
                })?;
            }
            debug!("[prctl] get pdeath_signal={}", sig);
            Ok(0)
        }
        PR_GET_DUMPABLE => Ok(0),
        PR_SET_DUMPABLE => {
            // 仅接受 SUID_DUMP_DISABLE(0) / SUID_DUMP_USER(1)
            if arg2 > 1 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_SET_TIMING => {
            // 仅支持 PR_TIMING_STATISTICAL(0)
            Err(SysErrNo::EINVAL)
        }
        PR_SET_TIMERSLACK => {
            // Zero restores Linux's default 50us timer slack.
            task.inner_lock().timer_slack_ns = if arg2 == 0 { 50_000 } else { arg2 as usize };
            Ok(0)
        }
        PR_GET_TIMERSLACK => Ok(task.inner_lock().timer_slack_ns),
        PR_SET_NAME => {
            // EFAULT: 非法地址
            if (arg2 as isize) <= 0 || if_bad_address(arg2 as usize) {
                return Err(SysErrNo::EFAULT);
            }
            Ok(0)
        }
        PR_GET_SECCOMP => Ok(task.inner_lock().seccomp_state.mode()),
        PR_SET_SECCOMP => match arg2 {
            SECCOMP_MODE_STRICT => {
                // prctl(2) is variadic. Linux ignores the unused argument
                // registers here, and libc does not reliably clear them.
                let mut inner = task.inner_lock();
                if !inner.seccomp_state.is_disabled() {
                    return Err(SysErrNo::EINVAL);
                }
                inner.seccomp_state = SeccompState::Strict;
                Ok(0)
            }
            SECCOMP_MODE_FILTER => {
                if !task.inner_lock().seccomp_state.is_disabled() {
                    return Err(SysErrNo::EINVAL);
                }
                let no_new_privs = task.inner_lock().no_new_privs;
                if !no_new_privs && !current_has_cap_sys_admin() {
                    return Err(SysErrNo::EACCES);
                }
                if arg3 == 0 || if_bad_address(arg3) {
                    return Err(SysErrNo::EFAULT);
                }

                let memory_set = task.process.memory_set_arc();
                let fprog = copy_from_user_val(&memory_set, arg3 as *const SockFprog)?;
                let filter_len = fprog.len as usize;
                if filter_len == 0 || filter_len > SECCOMP_FILTER_MAX_INSNS {
                    return Err(SysErrNo::EINVAL);
                }
                if fprog.filter.is_null() || if_bad_address(fprog.filter as usize) {
                    return Err(SysErrNo::EFAULT);
                }

                let mut filter = alloc::vec![SockFilter::default(); filter_len];
                let filter_bytes = unsafe {
                    core::slice::from_raw_parts_mut(
                        filter.as_mut_ptr() as *mut u8,
                        filter_len * size_of::<SockFilter>(),
                    )
                };
                copy_from_user(&memory_set, fprog.filter as usize, filter_bytes)?;
                let seccomp_state = SeccompState::new_filter(filter).ok_or(SysErrNo::EINVAL)?;

                let mut inner = task.inner_lock();
                if !inner.seccomp_state.is_disabled() {
                    return Err(SysErrNo::EINVAL);
                }
                inner.seccomp_state = seccomp_state;
                Ok(0)
            }
            _ => Err(SysErrNo::EINVAL),
        },
        PR_CAPBSET_READ => Ok(0),
        PR_CAPBSET_DROP => {
            // 没有 CAP_SETPCAP → EPERM
            Err(SysErrNo::EPERM)
        }
        PR_SET_SECUREBITS => {
            // 没有 CAP_SETPCAP → EPERM
            Err(SysErrNo::EPERM)
        }
        PR_MCE_KILL => {
            let mut inner = task.inner_lock();
            match arg2 {
                PR_MCE_KILL_CLEAR => {
                    inner.mce_kill_policy = PR_MCE_KILL_DEFAULT;
                    Ok(0)
                }
                PR_MCE_KILL_SET => match arg3 as u32 {
                    PR_MCE_KILL_LATE | PR_MCE_KILL_EARLY | PR_MCE_KILL_DEFAULT => {
                        inner.mce_kill_policy = arg3 as u32;
                        Ok(0)
                    }
                    _ => Err(SysErrNo::EINVAL),
                },
                _ => Err(SysErrNo::EINVAL),
            }
        }
        PR_MCE_KILL_GET => Ok(task.inner_lock().mce_kill_policy as usize),
        PR_SET_CHILD_SUBREAPER => {
            // Linux 将任意非零值规范化为 true，未使用的参数不参与校验。
            task.process.set_child_subreaper(arg2 != 0);
            debug!("[prctl] set child_subreaper={}", arg2 != 0);
            Ok(0)
        }
        PR_GET_CHILD_SUBREAPER => {
            let is_child_subreaper = task.process.is_child_subreaper() as i32;
            let memory_set = task.process.memory_set_arc();
            copy_to_user_val(&memory_set, arg2 as *mut i32, &is_child_subreaper)?;
            debug!("[prctl] get child_subreaper={}", is_child_subreaper);
            Ok(0)
        }
        PR_SET_NO_NEW_PRIVS => {
            // arg2 必须为 1 且 arg3/4/5 必须为 0
            if arg2 != 1 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            task.inner_lock().no_new_privs = true;
            Ok(0)
        }
        PR_GET_NO_NEW_PRIVS => {
            // arg2/3/4/5 必须为 0
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(task.inner_lock().no_new_privs as usize)
        }
        PR_SET_THP_DISABLE => {
            // arg3/4/5 必须为 0
            if arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_GET_THP_DISABLE => {
            // arg2/3/4/5 必须为 0
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_CAP_AMBIENT => {
            // 简化: 所有有效调用返回 0，无效参数返回 EINVAL
            Ok(0)
        }
        PR_GET_SPECULATION_CTRL => {
            // arg3/4/5 必须为 0
            if arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        _ => {
            // 未知选项
            warn!("[prctl] unsupported option {}", option);
            Err(SysErrNo::EINVAL)
        }
    }
}
