use alloc::sync::Arc;
use log::debug;

use crate::{
    mm::{get_data, put_data, safe_get_data},
    signal::{
        restore_frame, send_access_signal, send_signal_to_thread, send_signal_to_thread_group,
        send_signal_to_thread_of_proc, KSigAction, SigAction, SigInfo, SigSet, SIG_MAX_NUM,
    },
    syscall::{process, SignalMaskFlag},
    task::{current_task, exit_current_and_run_next, suspend_current_and_run_next},
    timer::Timespec,
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/rt_sigaction.2.html
pub fn sys_rt_sigaction(
    signo: usize,
    act: *const SigAction,
    old_act: *mut SigAction,
) -> SyscallRet {
    debug!(
        "[sys_rt_sigaction] signo is {}, act is {:?}, old_act is {:?}",
        signo, act, old_act
    );
    if signo > SIG_MAX_NUM {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let sigtable = process.get_locked_sigtable();
    let token = process.get_locked_memory_set_read().token();
    if old_act as usize != 0 {
        let sig_act = sigtable.action(signo).act;
        put_data(token, old_act, sig_act);
    }
    if act as usize != 0 {
        let new_act = get_data(token, act);
        debug!(
            "[sys_rt_sigaction] signo is {}, sig is {:?}, act is {:?}",
            signo,
            SigSet::from_sig(signo),
            new_act
        );
        let new_sig: KSigAction = if new_act.sa_handler == 0 {
            KSigAction::new(signo, false)
        } else if new_act.sa_handler == 1 {
            // 忽略
            KSigAction::ignore()
        } else {
            let customed = new_act.sa_handler != exit_current_and_run_next as *const () as usize;
            KSigAction {
                act: new_act,
                customed,
            }
        };
        sigtable.set_action(signo, new_sig);
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/rt_sigreturn.2.html
pub fn sys_rt_sigreturn() -> SyscallRet {
    restore_frame()
}

/// 参考 https://man7.org/linux/man-pages/man2/rt_sigprocmask.2.html
pub fn sys_rt_sigprocmask(how: u32, set: *const SigSet, old_set: *mut SigSet) -> SyscallRet {
    let task = current_task().unwrap();
    // debug!("strong count: {}", Arc::strong_count(&task));
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut task_inner = task.inner_lock();
    let how = SignalMaskFlag::from_bits(how).ok_or(SysErrNo::EINVAL)?;

    debug!(
        "oldset is {:x}, set is {:x}",
        old_set as usize, set as usize
    );
    if old_set as usize != 0 {
        put_data(memory_set.token(), old_set, task_inner.sig_mask);
    }
    if set as usize != 0 {
        let mask = safe_get_data(&*memory_set, set);

        // debug!(
        //     "[sys_sigprocmask] how is {:?}, mask is {:?}, old_set is {:x}",
        //     how, mask, old_set as usize
        // );

        // let mut blocked = &mut task_inner.sig_mask;
        match how {
            SignalMaskFlag::SIG_BLOCK => task_inner.sig_mask |= mask,
            SignalMaskFlag::SIG_UNBLOCK => task_inner.sig_mask &= !mask,
            SignalMaskFlag::SIG_SETMASK => task_inner.sig_mask = mask,
            _ => return Err(SysErrNo::EINVAL),
        }
    }
    Ok(0)
}

/// 在指定时间内挂起给定信号
/// 参考 https://man7.org/linux/man-pages/man2/rt_sigtimedwait.2.html
pub fn sys_rt_sigtimedwait(
    _sig: *const SigSet,
    _info: *mut SigInfo,
    _timeout: *const Timespec,
) -> SyscallRet {
    // TODO(ZMY): Linux实现与POSIX标准不同,只在部分pthread测试使用过,伪实现
    Ok(0)
}

/// 暂时将调用线程的信号掩码替换为 mask 给出的掩码，然后暂停线程，直到传递信号，
/// 其操作是调用信号处理程序或终止进程
/// mask 用于屏蔽特定的信号，直到有不同的新信号传入
/// 参考 https://man7.org/linux/man-pages/man2/rt_sigsuspend.2.html
pub fn sys_rt_sigsuspend(mask: *const SigSet) -> SyscallRet {
    // TODO(ZMY): 暂停线程
    debug!("[sys_rt_sigsuspend] mask is {:?}", mask);
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();
    let mask = get_data(token, mask);
    let old_mask = task_inner.sig_mask;
    task_inner.sig_mask = mask;
    drop(task_inner);
    drop(task);
    loop {
        let task = current_task().unwrap();
        let mut task_inner = task.inner_lock();
        let pending = task_inner.sig_pending.difference(task_inner.sig_mask); // 修复bug, 改为判断待处理信号和掩码信号的不同，
        if !pending.is_empty() {
            // 发生中断
            debug!("[sys_rt_sigsuspend] pending is {:?}", pending);
            task_inner.sig_mask = old_mask;
            return Err(SysErrNo::EINTR);
        }
        drop(task_inner);
        drop(task);
        suspend_current_and_run_next();
    }
}

// pid == 0 then sig is sent to every process in the process group of current process
/// pid == -1 then sig is sent to every process which current process has permission ( except init proc )
/// pid > 0 then sig is sent to the process with the ID specified by pid
/// pid < -1 the sig is sent to every process in process group whose ID is -pid
/// 参考 https://man7.org/linux/man-pages/man2/kill.2.html
pub fn sys_kill(pid: isize, signo: usize) -> SyscallRet {
    if signo == 0 {
        return Ok(0);
    }
    let sig = SigSet::from_sig(signo);

    debug!("[sys_kill] pid is {}, sig is {:?}", pid, sig);

    let ret = match pid {
        _ if pid > 0 => send_signal_to_thread_group(pid as usize, sig),
        0 => send_signal_to_thread_group(current_task().unwrap().pid(), sig),
        -1 => send_access_signal(current_task().unwrap().tid(), sig),
        _ => send_signal_to_thread_group(-pid as usize, sig),
    };
    ret
}

/// 参考 https://man7.org/linux/man-pages/man2/tkill.2.html
pub fn sys_tkill(tid: usize, signo: usize) -> SyscallRet {
    let sig = SigSet::from_sig(signo);
    debug!("[sys_tkill] thread {} receive signal {:?}", tid, sig);
    send_signal_to_thread(tid, sig);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/tgkill.2.html
pub fn sys_tgkill(tgid: usize, tid: usize, signo: usize) -> SyscallRet {
    let sig = SigSet::from_sig(signo);

    debug!(
        "[sys_tgkill] tgid is {}, tid is {}, sig is {:?}",
        tgid, tid, sig
    );

    send_signal_to_thread_of_proc(tgid, tid, sig);
    Ok(0)
}
