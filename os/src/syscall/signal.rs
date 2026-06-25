use core::{future::poll_fn, task::Poll};

use alloc::sync::Arc;
use log::{debug, error};

use crate::{
    mm::{copy_from_user, copy_to_user},
    signal::{
        restore_frame, send_access_signal, send_signal_to_thread, send_signal_to_thread_group,
        send_signal_to_thread_of_proc, KSigAction, SigAction, SigInfo, SigSet, SIGCONT, SIGKILL,
        SIGSTOP, SIG_MAX_NUM,
    },
    syscall::SignalMaskFlag,
    task::{block_on, current_task, exit_current_and_run_next, suspend_current_and_run_next},
    timer::{add_sigtimedwait_timer, get_time_spec, Timespec},
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/rt_sigaction.2.html
pub fn sys_rt_sigaction(
    signo: usize,
    act: *const SigAction,
    old_act: *mut SigAction,
) -> SyscallRet {
    debug!(
        "[sys_rt_sigaction] signo is {:#b}, act is {:?}, old_act is {:?}",
        signo, act, old_act
    );
    // signo == 0 用于检查进程是否存在，这里不允许
    if signo == 0 || signo > SIG_MAX_NUM {
        error!("invalid signo: {}", signo);
        return Err(SysErrNo::EINVAL);
    }
    // SIGKILL 和 SIGSTOP 不可被捕获、忽略或重设；只允许 act == NULL 查询旧 action。
    if (signo == SIGKILL || signo == SIGSTOP) && act as usize != 0 {
        debug!("[sys_rt_sigaction] attempt to change SIGKILL/SIGSTOP action");
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let sigtable = process.get_locked_sigtable();
    let memory_set = process.get_locked_memory_set_read();
    if old_act as usize != 0 {
        let sig_act = sigtable.action(signo).act;
        copy_to_user(&memory_set, old_act as usize, unsafe {
            core::slice::from_raw_parts(
                &sig_act as *const SigAction as *const u8,
                core::mem::size_of::<SigAction>(),
            )
        })?;
    }
    if act as usize != 0 {
        let mut new_act: SigAction = unsafe { core::mem::zeroed() };
        copy_from_user(&memory_set, act as usize, unsafe {
            core::slice::from_raw_parts_mut(
                &mut new_act as *mut SigAction as *mut u8,
                core::mem::size_of::<SigAction>(),
            )
        })?;
        debug!(
            "[sys_rt_sigaction] signo is {}, sig is {:?}, act is {:?}",
            signo,
            SigSet::from_sig(signo),
            new_act
        );
        let new_sig: KSigAction = if new_act.sa_handler == 0 {
            // SIG_DFL: 恢复默认行为
            KSigAction::new(signo, false)
        } else if new_act.sa_handler == 1 {
            // SIG_IGN: 忽略信号
            KSigAction::ignore()
        } else {
            // 用户自定义处理函数
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
    let process = task.process.inner_lock();
    let memory_set = &*process.get_locked_memory_set_read();
    let mut task_inner = task.inner_lock();
    let how = SignalMaskFlag::from_bits(how).ok_or(SysErrNo::EINVAL)?;

    if old_set as usize != 0 {
        copy_to_user(memory_set, old_set as usize, unsafe {
            core::slice::from_raw_parts(
                &task_inner.sig_mask as *const SigSet as *const u8,
                core::mem::size_of::<SigSet>(),
            )
        })?;
    }
    if set as usize != 0 {
        let mut mask: SigSet = SigSet::default();
        copy_from_user(memory_set, set as usize, unsafe {
            core::slice::from_raw_parts_mut(
                &mut mask as *mut SigSet as *mut u8,
                core::mem::size_of::<SigSet>(),
            )
        })?;

        debug!(
            "[sys_sigprocmask] how is {:?}, mask is {:?}, old_set is {:x}",
            how, mask, old_set as usize
        );

        mask.remove(SigSet::SIGKILL | SigSet::SIGSTOP);
        match how {
            SignalMaskFlag::SIG_BLOCK => task_inner.sig_mask |= mask,
            SignalMaskFlag::SIG_UNBLOCK => task_inner.sig_mask &= !mask,
            SignalMaskFlag::SIG_SETMASK => {
                task_inner.sig_mask = mask;
            }
            _ => return Err(SysErrNo::EINVAL),
        }
        task_inner
            .sig_mask
            .remove(SigSet::SIGKILL | SigSet::SIGSTOP);
    }
    Ok(0)
}

/// 参考 https://www.man7.org/linux/man-pages/man2/sigpending.2.html
pub fn sys_rt_sigpending(set: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let sig_pending = task_inner.sig_pending;
    copy_to_user(&memory_set, set, unsafe {
        core::slice::from_raw_parts(
            &sig_pending as *const SigSet as *const _,
            core::mem::size_of::<SigSet>(),
        )
    });
    Ok(0)
}

/// 在指定时间内等待信号集中的任一信号变为 pending 状态
/// 返回已消耗的信号编号；超时返回 EAGAIN
/// 参考 https://man7.org/linux/man-pages/man2/rt_sigtimedwait.2.html
pub fn sys_rt_sigtimedwait(
    set_ptr: *const SigSet,
    info_ptr: *mut SigInfo,
    timeout_ptr: *const Timespec,
) -> SyscallRet {
    // 清除上一次遗留的超时标记
    {
        let task = current_task().unwrap();
        task.inner_lock().sigtimedwait_timedout = false;
    }
    // 从用户空间拷贝信号集和超时参数（需要先获取 memory_set）
    let sigset = {
        let task = current_task().unwrap();
        let proc_inner = task.process.inner_lock();
        let memory_set = proc_inner.get_locked_memory_set_read();

        let mut sigset: SigSet = SigSet::default();
        if set_ptr as usize != 0 {
            copy_from_user(&memory_set, set_ptr as usize, unsafe {
                core::slice::from_raw_parts_mut(
                    &mut sigset as *mut SigSet as *mut u8,
                    core::mem::size_of::<SigSet>(),
                )
            })?;
        }
        // 处理超时参数并注册定时器
        if !timeout_ptr.is_null() {
            if timeout_ptr as usize == usize::MAX {
                return Err(SysErrNo::EINVAL);
            }
            let mut ts: Timespec = Timespec::default();
            copy_from_user(&memory_set, timeout_ptr as usize, unsafe {
                core::slice::from_raw_parts_mut(
                    &mut ts as *mut Timespec as *mut u8,
                    core::mem::size_of::<Timespec>(),
                )
            })?;
            if ts.tv_nsec >= 1_000_000_000 {
                return Err(SysErrNo::EINVAL);
            }
            let now = get_time_spec();
            let expire_time = now + ts;
            let task = current_task().unwrap();
            add_sigtimedwait_timer(expire_time, &task);
        }
        sigset
    };
    // memory_set 和 proc_inner 已在这里 drop，后续不需要再持有进程锁
    block_on(poll_fn(|cx| {
        let task = current_task().unwrap();
        let mut task_inner = task.inner_lock();

        // 注册 waker：定时器到期时通过 task.interrupt() 唤醒
        task.interrupt_waker.register(cx.waker());

        // 检查是否因超时被唤醒
        if task_inner.sigtimedwait_timedout {
            task_inner.sigtimedwait_timedout = false;
            task.clear_interrupt(); // 清除 timer 设置的 interrupted 标志，避免污染后续 syscall
            drop(task_inner);
            return Poll::Ready(Err(SysErrNo::EAGAIN));
        }

        // 查找 sigset 中已有的待处理信号（不检查 sig_mask；
        // POSIX 要求调用前已将 set 中的信号 block，故 pending 中的
        // 目标信号必然被屏蔽，不应被 difference 排除）
        let matched = sigset & task_inner.sig_pending;
        if !matched.is_empty() {
            // 取出编号最小的匹配信号
            let signo = matched.peek_front().unwrap();
            let signal = SigSet::from_sig(signo);
            // 从 pending 集合中消耗该信号
            task_inner.sig_pending.remove(signal);
            // 填充 siginfo
            if info_ptr as usize != 0 {
                let sig_info = SigInfo::new(
                    signo as u32,
                    0,
                    (-1i32) as u32, // si_code: SI_QUEUE
                    task.pid() as u32,
                );
                let proc_inner = task.process.inner_lock();
                let mem_set = proc_inner.get_locked_memory_set_read();
                copy_to_user(&mem_set, info_ptr as usize, unsafe {
                    core::slice::from_raw_parts(
                        &sig_info as *const SigInfo as *const u8,
                        core::mem::size_of::<SigInfo>(),
                    )
                })?;
            }

            drop(task_inner);
            return Poll::Ready(Ok(signo));
        }

        drop(task_inner);
        Poll::Pending
    }))
}

/// 暂时将调用线程的信号掩码替换为 mask 给出的掩码，然后暂停线程，直到传递信号，
/// 其操作是调用信号处理程序或终止进程
/// mask 用于屏蔽特定的信号，直到有不同的新信号传入
/// 参考 https://man7.org/linux/man-pages/man2/rt_sigsuspend.2.html
pub fn sys_rt_sigsuspend(mask: *const SigSet) -> SyscallRet {
    // TODO(ZMY): 暂停线程
    // debug!("[sys_rt_sigsuspend] mask is {:?}", mask);
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut mask_val: SigSet = SigSet::default();
    copy_from_user(&memory_set, mask as usize, unsafe {
        core::slice::from_raw_parts_mut(
            &mut mask_val as *mut SigSet as *mut u8,
            core::mem::size_of::<SigSet>(),
        )
    })?;
    let mut mask = mask_val;
    mask.remove(SigSet::SIGKILL | SigSet::SIGSTOP);
    let old_mask = task_inner.sig_mask;
    task_inner.sig_mask = mask;
    drop(task_inner);
    loop {
        let task = current_task().unwrap();
        let mut task_inner = task.inner_lock();
        let pending = task_inner.sig_pending.difference(task_inner.sig_mask); // 修复bug, 改为判断待处理信号和掩码信号的不同，
        if !pending.is_empty() {
            // 发生中断
            // debug!("[sys_rt_sigsuspend] pending is {:?}", pending);
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
    if signo > SIG_MAX_NUM {
        return Err(SysErrNo::EINVAL);
    }
    let sig = SigSet::from_sig(signo);

    // debug!("[sys_kill] pid is {}, sig is {:?}", pid, sig);

    let ret = match pid {
        _ if pid > 0 => send_signal_to_thread_group(pid as usize, sig),
        0 => send_signal_to_thread_group(current_task().unwrap().pid(), sig),
        -1 => send_access_signal(current_task().unwrap().tid(), sig),
        _ => send_signal_to_thread_group(-pid as usize, sig),
    };

    // SIGCONT 恢复停止态任务后，让出一次 CPU，使被恢复的任务有机会先处理
    // pending SIGCONT 并继续运行；否则父进程可能马上执行后续同步操作。
    let resumed = ret?;
    if signo == SIGCONT && resumed > 0 {
        suspend_current_and_run_next();
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/tkill.2.html
pub fn sys_tkill(tid: usize, signo: usize) -> SyscallRet {
    if signo == 0 {
        return Ok(0);
    }
    if signo > SIG_MAX_NUM {
        return Err(SysErrNo::EINVAL);
    }
    let sig = SigSet::from_sig(signo);
    // debug!("[sys_tkill] thread {} receive signal {:?}", tid, sig);
    send_signal_to_thread(tid, sig);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/tgkill.2.html
pub fn sys_tgkill(tgid: usize, tid: usize, signo: usize) -> SyscallRet {
    if signo == 0 {
        return Ok(0);
    }
    if signo > SIG_MAX_NUM {
        return Err(SysErrNo::EINVAL);
    }
    let sig = SigSet::from_sig(signo);

    // debug!(
    //     "[sys_tgkill] tgid is {}, tid is {}, sig is {:?}",
    //     tgid, tid, sig
    // );

    send_signal_to_thread_of_proc(tgid, tid, sig);
    Ok(0)
}
