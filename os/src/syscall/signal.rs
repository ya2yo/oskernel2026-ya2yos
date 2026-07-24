use core::{future::poll_fn, task::Poll};

use alloc::sync::Arc;
use log::{debug, error};

use crate::{
    mm::{copy_from_user, copy_from_user_val, copy_to_user, copy_to_user_val},
    signal::{
        restore_frame, send_signal_to_thread_group, send_user_signal_to_accessible_processes,
        send_user_signal_to_process_group, send_user_signal_to_thread,
        send_user_signal_to_thread_group, send_user_signal_to_thread_of_proc, KSigAction,
        SigAction, SigActionFlags, SigInfo, SigSet, SignalStack, SIGCONT, SIGKILL, SIGSTOP,
        SIG_DFL, SIG_IGN, SIG_MAX_NUM,
    },
    syscall::SignalMaskFlag,
    task::{block_current_and_run_next, block_on, current_task, suspend_current_and_run_next},
    timer::{add_sigtimedwait_timer, get_time_spec, Timespec},
    utils::{SysErrNo, SyscallRet},
};

#[repr(C)]
#[derive(Clone, Copy)]
#[cfg(target_arch = "riscv64")]
struct RawSigAction {
    handler: usize,
    flags: usize,
    restorer: usize,
    mask: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
#[cfg(not(target_arch = "riscv64"))]
struct RawSigAction {
    handler: usize,
    flags: usize,
    mask: [u32; 2],
    unused: usize,
}

impl RawSigAction {
    fn from_sigaction(act: SigAction) -> Self {
        let mask = act.sa_mask.bits();
        Self {
            handler: act.sa_handler,
            flags: act.sa_flags.bits() as usize,
            #[cfg(target_arch = "riscv64")]
            restorer: act.sa_restore,
            mask: [mask as u32, (mask >> 32) as u32],
            #[cfg(not(target_arch = "riscv64"))]
            unused: 0,
        }
    }

    fn into_sigaction(self) -> SigAction {
        let mask = self.mask[0] as usize | ((self.mask[1] as usize) << 32);
        SigAction {
            sa_handler: self.handler,
            sa_flags: SigActionFlags::from_bits_truncate(self.flags as u32),
            #[cfg(target_arch = "riscv64")]
            sa_restore: self.restorer,
            #[cfg(not(target_arch = "riscv64"))]
            sa_restore: 0,
            sa_mask: SigSet::from_bits_truncate(mask),
        }
    }
}

/// https://www.man7.org/linux/man-pages/man2/sigaltstack.2.html
/// Linux sigaltstack(2): configure or query the calling thread's alternate
/// signal stack.  The saved state is task-local because sibling threads do not
/// share alternate stacks even when they share a process address space.
pub fn sys_sigaltstack(new_stack: *const SignalStack, old_stack: *mut SignalStack) -> SyscallRet {
    let task = current_task().unwrap();

    // Match Linux ordering: an unreadable input fails before any state change
    // or old-stack copy; an unwritable output may fail after a successful set.
    let requested = if new_stack.is_null() {
        None
    } else {
        let memory_set = task.process.memory_set_arc();
        Some(copy_from_user_val(&*memory_set, new_stack)?)
    };

    let previous = {
        let mut task_inner = task.inner_lock();
        let user_sp = task_inner.trap_cx().get_sp();
        let previous = task_inner.alt_signal_stack.user_view(user_sp);
        if let Some(requested) = requested {
            task_inner.alt_signal_stack = task_inner
                .alt_signal_stack
                .replace_from_user(requested, user_sp)?;
        }
        previous
    };

    if !old_stack.is_null() {
        let memory_set = task.process.memory_set_arc();
        copy_to_user_val(&*memory_set, old_stack, &previous)?;
    }

    Ok(0)
}

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
    let process = &task.process;
    let memory_set = process.memory_set_arc();
    if old_act as usize != 0 {
        let sig_act = process.with_sigtable(|sigtable| sigtable.action(signo).act);
        let raw = RawSigAction::from_sigaction(sig_act);
        copy_to_user_val(&memory_set, old_act as *mut RawSigAction, &raw)?;
    }
    if act as usize != 0 {
        let raw: RawSigAction = copy_from_user_val(&memory_set, act as *const RawSigAction)?;
        let mut new_act = raw.into_sigaction();
        new_act.sa_mask.remove(SigSet::SIGKILL | SigSet::SIGSTOP);
        debug!(
            "[sys_rt_sigaction] signo is {}, sig is {:?}, act is {:?}",
            signo,
            SigSet::from_sig(signo),
            new_act
        );
        let new_sig: KSigAction = if new_act.sa_handler == SIG_DFL {
            // SIG_DFL: 恢复默认行为
            KSigAction::default_action()
        } else if new_act.sa_handler == SIG_IGN {
            // SIG_IGN: 忽略信号
            KSigAction::ignore()
        } else {
            // 用户自定义处理函数
            KSigAction::handler(new_act)
        };
        process.with_sigtable(|sigtable| {
            sigtable.set_action(signo, new_sig);
        });
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
    let process = &task.process;
    let memory_set = &*process.memory_set_arc();
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
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
    let sig_pending = task_inner.sig_pending;
    copy_to_user(&memory_set, set, unsafe {
        core::slice::from_raw_parts(
            &sig_pending as *const SigSet as *const _,
            core::mem::size_of::<SigSet>(),
        )
    })?;
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
        let proc_inner = &task.process;
        let memory_set = proc_inner.memory_set_arc();

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
            // 清除 timer 设置的 interrupted 标志和本次等待注册的 waker，
            // 避免污染后续 syscall。
            task.clear_interrupt_waiter();
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
            let sig_info = task_inner.sig_pending_info[signo]
                .take()
                .unwrap_or_else(|| SigInfo::new(signo as u32, 0, 0, 0));
            // 本次 sigtimedwait 可能是被 add_signal() 通过
            // wake_interruptible() 唤醒的；成功消费信号后必须清掉内部
            // interrupted 标志，否则后续 wait4/select 等 interruptible
            // syscall 会误返回 EINTR。
            task.clear_interrupt_waiter();
            // 填充 siginfo
            if info_ptr as usize != 0 {
                let proc_inner = &task.process;
                let mem_set = proc_inner.memory_set_arc();
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
    // debug!("[sys_rt_sigsuspend] mask is {:?}", mask);
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    let process = &task.process;
    let memory_set = process.memory_set_arc();
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
        let task_inner = task.inner_lock();
        let pending = task_inner.sig_pending.difference(task_inner.sig_mask);
        if !pending.is_empty() {
            // 先检查 pending signal 是否是可忽略的（SIG_IGN 或默认忽略动作，
            // 例如默认的 SIGCHLD）。可忽略信号不应导致 sigsuspend 返回 EINTR。
            drop(task_inner);
            drop(task);
            if crate::signal::consume_ignorable_pending_signal_for_current_task() {
                continue;
            }
            let task = current_task().unwrap();
            let mut task_inner = task.inner_lock();
            // 保留临时 mask，确保 trap_return 仍能选中刚刚唤醒 sigsuspend
            // 的信号。handler 路径由 setup_frame 将旧 mask 写入 signal frame，
            // 无 handler 路径则由 trap_return 恢复。
            task_inner.sigsuspend_restore_mask = Some(old_mask);
            return Err(SysErrNo::EINTR);
        }
        drop(task_inner);
        drop(task);
        block_current_and_run_next();
    }
}

// pid == 0 then sig is sent to every process in the process group of current process
/// pid == -1 then sig is sent to every process which current process has permission ( except init proc )
/// pid > 0 then sig is sent to the process with the ID specified by pid
/// pid < -1 the sig is sent to every process in process group whose ID is -pid
/// 参考 https://man7.org/linux/man-pages/man2/kill.2.html
pub fn sys_kill(pid: isize, signo: usize) -> SyscallRet {
    if signo > SIG_MAX_NUM {
        return Err(SysErrNo::EINVAL);
    }
    let sig = if signo == 0 {
        SigSet::empty()
    } else {
        SigSet::from_sig(signo)
    };

    // debug!("[sys_kill] pid is {}, sig is {:?}", pid, sig);

    let ret = match pid {
        _ if pid > 0 => send_user_signal_to_thread_group(pid as usize, sig, signo),
        0 => send_user_signal_to_process_group(current_task().unwrap().process.pgid(), sig, signo),
        -1 => send_user_signal_to_accessible_processes(sig, signo),
        _ => send_user_signal_to_process_group((-pid) as usize, sig, signo),
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
    send_user_signal_to_thread(tid, sig, signo);
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

    send_user_signal_to_thread_of_proc(tgid, tid, sig, signo);
    Ok(0)
}
