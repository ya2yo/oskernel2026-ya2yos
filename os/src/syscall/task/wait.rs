use core::{future::poll_fn, task::Poll};

use alloc::{sync::Arc, vec::Vec};
use linux_raw_sys::general::{
    CLD_CONTINUED, CLD_DUMPED, CLD_EXITED, CLD_KILLED, CLD_STOPPED, P_ALL, P_PGID, P_PID, P_PIDFD,
};
use log::debug;

use crate::{
    mm::copy_to_user,
    signal::{
        check_if_any_sig_for_current_task, SigActionFlags, SigInfo, SigOp, SigSet, SIGCHLD, SIG_IGN,
    },
    syscall::options::WaitOption,
    task::{block_on, current_task, interruptible, suspend_current_and_run_next, Process},
    utils::{SysErrNo, SyscallRet},
};

#[derive(Debug, Clone, Copy)]
enum WaitPid {
    /// Wait for any child process (pid == -1 or the effective pid == 0).
    Any,
    /// Wait for the child whose process ID is equal to the value (pid > 0).
    Pid(usize),
    /// Wait for any child whose process group ID equals this value.
    Pgid(u32),
    Tgid(u32),
    Sid(u32),
    Max,
}

impl WaitPid {
    /// 判断一个子进程是否属于本次 wait 请求的目标集合。
    ///
    /// `sys_waitpid()` 会把 pid 参数、`sys_waitid()` 会把 idtype/id 参数
    /// 先转换成 `WaitPid`，后续扫描 children 时统一调用这个函数过滤。
    fn apply(&self, child: &Process) -> bool {
        match self {
            WaitPid::Any => true,
            WaitPid::Pid(pid) => child.pid == *pid,
            WaitPid::Pgid(pgid) => child.pgid() == *pgid as usize,
            _ => false,
        }
    }
}

fn wait_status_from_exit_code(exit_code: i32, termination_signal: Option<(usize, bool)>) -> i32 {
    if let Some((signo, dumped_core)) = termination_signal {
        signo as i32 | if dumped_core { 0x80 } else { 0 }
    } else {
        exit_code << 8
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/waitpid.2.html
pub fn sys_waitpid(pid: i32, wstatus: *mut i32, options: u32) -> SyscallRet {
    let options = WaitOption::from_bits_truncate(options);
    debug!("sys_waitpid <= pid: {pid:?}, options: {options:?}");

    if pid == i32::MIN {
        return Err(SysErrNo::ESRCH);
    }

    // Interpret the `pid` argument per POSIX waitpid() semantics:
    //
    //   pid > 0   : wait for the specific child with that PID.
    //   pid == 0  : wait for any child in the caller's process group.
    //   pid == -1 : wait for any child (most common).
    //   pid < -1  : wait for any child whose process group ID equals -pid.
    //
    let wait_pid = match pid {
        ..=-2 => WaitPid::Pgid((-pid) as u32),
        -1 => WaitPid::Any,
        0 => WaitPid::Pgid(current_task().ok_or(SysErrNo::ESRCH)?.process.pgid() as u32),
        1.. => WaitPid::Pid(pid as usize),
    };

    block_on(interruptible(poll_fn(|cx| {
        let task = current_task().unwrap();
        let mut process_meta = task.process.meta_lock();

        let all_weak_children: Vec<_> = process_meta
            .children
            .iter()
            .filter_map(|w| {
                w.upgrade().map(|c| {
                    let meta = c.meta_lock();
                    (
                        c.pid,
                        meta.exit_signal,
                        meta.tasks.iter().all(|x| x.upgrade().is_none()),
                    )
                })
            })
            .collect();
        debug!(
            "sys_waitpid: my children (pid, exit_sig, all_exited): {:?}",
            all_weak_children
        );

        let children: Vec<Arc<Process>> = process_meta
            .children
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|child| wait_pid.apply(child))
            .filter(|child| {
                // __WALL: wait for all children regardless of exit_signal
                if options.contains(WaitOption::__WALL) {
                    return true;
                }
                let child_exit_signal = child.meta_lock().exit_signal;
                if options.contains(WaitOption::__WCLONE) {
                    // __WCLONE: only wait for clone children (exit_signal == -1)
                    child_exit_signal == -1
                } else {
                    // default: only wait for children created with SIGCHLD
                    child_exit_signal == SIGCHLD as i32
                }
            })
            .collect();

        if children.is_empty() {
            return Poll::Ready(Err(SysErrNo::ECHILD));
        }

        let stopped = if options.intersects(WaitOption::WUNTRACED | WaitOption::WSTOPPED) {
            children.iter().find_map(|child| {
                child
                    .meta_lock()
                    .stopped_signal
                    .map(|signo| (Arc::clone(child), signo))
            })
        } else {
            None
        };

        let pair = if stopped.is_none() {
            children
                .iter()
                .find(|child| child.all_tasks_exited())
                .map(|child| Arc::clone(child))
        } else {
            None
        };

        drop(children);

        if let Some((child, stop_signal)) = stopped {
            let found_pid = child.pid;

            if !wstatus.is_null() {
                let proc_inner = task.process.inner_lock();
                let memory_set = proc_inner.get_locked_memory_set_read();
                let value = ((stop_signal as i32) << 8) | 0x7f;
                if copy_to_user(&memory_set, wstatus as usize, unsafe {
                    core::slice::from_raw_parts(
                        &value as *const i32 as *const u8,
                        core::mem::size_of::<i32>(),
                    )
                })
                .is_err()
                {
                    return Poll::Ready(Err(SysErrNo::EFAULT));
                }
            }

            child.meta_lock().stopped_signal = None;
            Poll::Ready(Ok(found_pid))
        } else if let Some(child) = pair {
            let found_pid = child.pid;
            let exit_code = child.group_exit_code();
            let (child_usage, termination_signal) = {
                let child_meta = child.meta_lock();
                (child_meta.usage, child_meta.termination_signal)
            };

            if !wstatus.is_null() {
                let proc_inner = task.process.inner_lock();
                let memory_set = proc_inner.get_locked_memory_set_read();
                let value = wait_status_from_exit_code(exit_code, termination_signal);
                if copy_to_user(&memory_set, wstatus as usize, unsafe {
                    core::slice::from_raw_parts(
                        &value as *const i32 as *const u8,
                        core::mem::size_of::<i32>(),
                    )
                })
                .is_err()
                {
                    return Poll::Ready(Err(SysErrNo::EFAULT));
                }
            }

            if !options.contains(WaitOption::WNOWAIT) {
                {
                    let mut task_inner = task.inner_lock();
                    task_inner.time_data.cutime += child_usage.utime + child_usage.cutime;
                    task_inner.time_data.cstime += child_usage.stime + child_usage.cstime;
                    task_inner.time_data.cmaxrss = task_inner
                        .time_data
                        .cmaxrss
                        .max(child_usage.maxrss)
                        .max(child_usage.cmaxrss);
                }
                if let Some(idx) = process_meta.children.iter().position(|child| {
                    child
                        .upgrade()
                        .map(|child| child.pid == found_pid)
                        .unwrap_or(false)
                }) {
                    process_meta.children.remove(idx);
                }
                drop(child);
                Process::remove_from_global_map(found_pid);
            }

            Poll::Ready(Ok(found_pid))
        } else if options.contains(WaitOption::WNOHANG) {
            Poll::Ready(Ok(0))
        } else {
            // Check for pending signals before going to sleep.
            // Ignorable signals (SIGCHLD, SIG_IGN, default=Ignore) are
            // consumed and the wait continues.
            // Signals with a custom handler return EINTR (unless SA_RESTART
            // is set, in which case we consume the signal and continue).
            // Default Term/Core signals without a handler are left pending.
            if let Some(signo) = check_if_any_sig_for_current_task() {
                drop(process_meta);
                let act = task
                    .process
                    .inner_lock()
                    .get_locked_sigtable()
                    .action(signo);
                let ignorable = signo == SIGCHLD
                    || act.act.sa_handler == SIG_IGN
                    || (!act.customed && SigSet::from_sig(signo).default_op() == SigOp::Ignore);
                if ignorable {
                    task.inner_lock()
                        .sig_pending
                        .remove(SigSet::from_sig(signo));
                    drop(task);
                    return Poll::Pending;
                }
                if act.customed {
                    if !act.act.sa_flags.contains(SigActionFlags::SA_RESTART) {
                        drop(task);
                        return Poll::Ready(Err(SysErrNo::EINTR));
                    }
                    task.inner_lock()
                        .sig_pending
                        .remove(SigSet::from_sig(signo));
                    drop(task);
                    return Poll::Pending;
                }
                drop(task);
                return Poll::Pending;
            }

            process_meta.child_exit_event.register(cx.waker());
            drop(process_meta);
            drop(task);
            Poll::Pending
        }
    })))?
}

/// https://www.man7.org/linux/man-pages/man2/wait.2.html
pub fn sys_waitid(idtype: i32, id: i32, infop: *mut SigInfo, options: i32) -> SyscallRet {
    // waitid 的 options 只能包含 WaitOption 已知位。至少要指定一种
    // 可等待事件；当前内核只真正产生 WEXITED 事件，WSTOPPED/WCONTINUED
    // 先按 Linux 参数校验规则接受。
    let options = WaitOption::from_bits(options as u32).ok_or(SysErrNo::EINVAL)?;
    if !(options.contains(WaitOption::WEXITED)
        || options.contains(WaitOption::WSTOPPED)
        || options.contains(WaitOption::WCONTINUED))
    {
        return Err(SysErrNo::EINVAL);
    }

    // idtype 是枚举值，不是 bitflags，不能用 from_bits_truncate。
    // 先把 Linux selector 转成本文件复用的 WaitPid，后续扫描 children
    // 时与 waitpid 共用一套过滤逻辑。
    let wait_pid = match idtype as u32 {
        P_ALL => WaitPid::Any,
        P_PID if id > 0 => WaitPid::Pid(id as usize),
        P_PID => return Err(SysErrNo::EINVAL),
        P_PGID if id == 0 => {
            WaitPid::Pgid(current_task().ok_or(SysErrNo::ESRCH)?.process.pgid() as u32)
        }
        P_PGID if id > 0 => WaitPid::Pgid(id as u32),
        P_PGID => return Err(SysErrNo::EINVAL),
        P_PIDFD => return Err(SysErrNo::EBADF),
        _ => return Err(SysErrNo::EINVAL),
    };

    debug!("sys_waitid <= idtype: {idtype:?}, id: {id:?}, options: {options:?}");

    block_on(interruptible(poll_fn(|cx| {
        let task = current_task().unwrap();
        let mut process_meta = task.process.meta_lock();

        // 先按 selector 与 __W* clone 过滤出本次 waitid 可以观察的子进程。
        // 如果一个都没有，说明调用者没有符合条件的 child，返回 ECHILD。
        let children: Vec<Arc<Process>> = process_meta
            .children
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|child| wait_pid.apply(child))
            .filter(|child| {
                if options.contains(WaitOption::__WALL) {
                    return true;
                }
                let child_exit_signal = child.meta_lock().exit_signal;
                if options.contains(WaitOption::__WCLONE) {
                    child_exit_signal == -1
                } else {
                    child_exit_signal == SIGCHLD as i32
                }
            })
            .collect();

        if children.is_empty() {
            return Poll::Ready(Err(SysErrNo::ECHILD));
        }

        let stopped = if options.contains(WaitOption::WSTOPPED) {
            children.iter().find_map(|child| {
                child
                    .meta_lock()
                    .stopped_signal
                    .map(|signo| (Arc::clone(child), signo))
            })
        } else {
            None
        };

        let continued = if stopped.is_none() && options.contains(WaitOption::WCONTINUED) {
            children.iter().find_map(|child| {
                child
                    .meta_lock()
                    .continued_signal
                    .map(|signo| (Arc::clone(child), signo))
            })
        } else {
            None
        };

        let pair =
            if stopped.is_none() && continued.is_none() && options.contains(WaitOption::WEXITED) {
                children
                    .iter()
                    .find(|child| child.all_tasks_exited())
                    .map(Arc::clone)
            } else {
                None
            };

        drop(children);

        if let Some((child, stop_signal)) = stopped {
            let found_pid = child.pid;
            if !infop.is_null() {
                let sig_info = SigInfo::new_child(
                    SIGCHLD as u32,
                    CLD_STOPPED,
                    found_pid as u32,
                    stop_signal as u32,
                );
                let proc_inner = task.process.inner_lock();
                let memory_set = proc_inner.get_locked_memory_set_read();
                if copy_to_user(&memory_set, infop as usize, unsafe {
                    core::slice::from_raw_parts(
                        &sig_info as *const SigInfo as *const u8,
                        core::mem::size_of::<SigInfo>(),
                    )
                })
                .is_err()
                {
                    return Poll::Ready(Err(SysErrNo::EFAULT));
                }
            }

            if !options.contains(WaitOption::WNOWAIT) {
                child.meta_lock().stopped_signal = None;
            }

            if infop.is_null() {
                Poll::Ready(Ok(found_pid))
            } else {
                Poll::Ready(Ok(0))
            }
        } else if let Some((child, cont_signal)) = continued {
            let found_pid = child.pid;
            if !infop.is_null() {
                let sig_info = SigInfo::new_child(
                    SIGCHLD as u32,
                    CLD_CONTINUED,
                    found_pid as u32,
                    cont_signal as u32,
                );
                let proc_inner = task.process.inner_lock();
                let memory_set = proc_inner.get_locked_memory_set_read();
                if copy_to_user(&memory_set, infop as usize, unsafe {
                    core::slice::from_raw_parts(
                        &sig_info as *const SigInfo as *const u8,
                        core::mem::size_of::<SigInfo>(),
                    )
                })
                .is_err()
                {
                    return Poll::Ready(Err(SysErrNo::EFAULT));
                }
            }

            if !options.contains(WaitOption::WNOWAIT) {
                child.meta_lock().continued_signal = None;
            }

            if infop.is_null() {
                Poll::Ready(Ok(found_pid))
            } else {
                Poll::Ready(Ok(0))
            }
        } else if let Some(child) = pair {
            let found_pid = child.pid;
            let exit_code = child.group_exit_code();
            let (child_usage, termination_signal) = {
                let child_meta = child.meta_lock();
                (child_meta.usage, child_meta.termination_signal)
            };

            // waitid 成功时返回值是 0，具体结果写入 siginfo_t。
            // 正常 exit 事件使用 SIGCHLD + CLD_EXITED，si_status 保存未左移的退出码。
            // 信号终止事件则返回原始信号号，并区分 CLD_KILLED / CLD_DUMPED。
            if !infop.is_null() {
                let (si_code, si_status) = if let Some((signo, dumped_core)) = termination_signal {
                    (
                        if dumped_core { CLD_DUMPED } else { CLD_KILLED },
                        signo as u32,
                    )
                } else {
                    (CLD_EXITED, exit_code as u32)
                };
                let sig_info =
                    SigInfo::new_child(SIGCHLD as u32, si_code, found_pid as u32, si_status);
                let proc_inner = task.process.inner_lock();
                let memory_set = proc_inner.get_locked_memory_set_read();
                if copy_to_user(&memory_set, infop as usize, unsafe {
                    core::slice::from_raw_parts(
                        &sig_info as *const SigInfo as *const u8,
                        core::mem::size_of::<SigInfo>(),
                    )
                })
                .is_err()
                {
                    return Poll::Ready(Err(SysErrNo::EFAULT));
                }
            }

            // WNOWAIT 表示只观察，不回收 zombie。否则与 waitpid 一样累计
            // RUSAGE_CHILDREN 相关时间/RSS，并从父进程 children 与全局 pid map 中移除。
            if !options.contains(WaitOption::WNOWAIT) {
                {
                    let mut task_inner = task.inner_lock();
                    task_inner.time_data.cutime += child_usage.utime + child_usage.cutime;
                    task_inner.time_data.cstime += child_usage.stime + child_usage.cstime;
                    task_inner.time_data.cmaxrss = task_inner
                        .time_data
                        .cmaxrss
                        .max(child_usage.maxrss)
                        .max(child_usage.cmaxrss);
                }
                if let Some(idx) = process_meta.children.iter().position(|child| {
                    child
                        .upgrade()
                        .map(|child| child.pid == found_pid)
                        .unwrap_or(false)
                }) {
                    process_meta.children.remove(idx);
                }
                drop(child);
                Process::remove_from_global_map(found_pid);
            }

            if infop.is_null() {
                Poll::Ready(Ok(found_pid))
            } else {
                Poll::Ready(Ok(0))
            }
        } else if options.contains(WaitOption::WNOHANG) {
            // Linux 规定 WNOHANG 且无可返回 child 时成功返回 0。
            // 为避免用户态读到旧内容，这里也把 infop 清零。
            if !infop.is_null() {
                let sig_info = SigInfo::new(0, 0, 0, 0);
                let proc_inner = task.process.inner_lock();
                let memory_set = proc_inner.get_locked_memory_set_read();
                if copy_to_user(&memory_set, infop as usize, unsafe {
                    core::slice::from_raw_parts(
                        &sig_info as *const SigInfo as *const u8,
                        core::mem::size_of::<SigInfo>(),
                    )
                })
                .is_err()
                {
                    return Poll::Ready(Err(SysErrNo::EFAULT));
                }
            }
            Poll::Ready(Ok(0))
        } else {
            // 没有可返回 child 且允许阻塞时，先按 waitpid 相同规则处理待决信号；
            // 再把当前任务注册到父进程的 child_exit_event，等待子进程退出唤醒。
            if let Some(signo) = check_if_any_sig_for_current_task() {
                drop(process_meta);
                let act = task
                    .process
                    .inner_lock()
                    .get_locked_sigtable()
                    .action(signo);
                let ignorable = signo == SIGCHLD
                    || act.act.sa_handler == SIG_IGN
                    || (!act.customed && SigSet::from_sig(signo).default_op() == SigOp::Ignore);
                if ignorable {
                    task.inner_lock()
                        .sig_pending
                        .remove(SigSet::from_sig(signo));
                    drop(task);
                    return Poll::Pending;
                }
                if act.customed {
                    if !act.act.sa_flags.contains(SigActionFlags::SA_RESTART) {
                        drop(task);
                        return Poll::Ready(Err(SysErrNo::EINTR));
                    }
                    task.inner_lock()
                        .sig_pending
                        .remove(SigSet::from_sig(signo));
                    drop(task);
                    return Poll::Pending;
                }
                drop(task);
                return Poll::Pending;
            }

            process_meta.child_exit_event.register(cx.waker());
            drop(process_meta);
            drop(task);
            Poll::Pending
        }
    })))?
}
