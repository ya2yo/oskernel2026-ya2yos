use core::{future::poll_fn, task::Poll};

use alloc::{sync::Arc, vec::Vec};
use log::debug;

use crate::{
    mm::copy_to_user,
    signal::{check_if_any_sig_for_current_task, SigActionFlags, SigOp, SigSet, SIGCHLD, SIG_IGN},
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
    /// Wait for any child whose process group ID equals the absolute value
    /// of pid (pid < -1).  Since the kernel does not yet track per‑process
    /// pgid, this filter currently accepts no children.  Once full job‑control
    /// is added, `ProcessMeta` will carry a `pgid` field and this match arm
    /// will compare against it.
    Pgid(u32),
}

impl WaitPid {
    fn apply(&self, child: &Process) -> bool {
        match self {
            WaitPid::Any => true,
            WaitPid::Pid(pid) => child.pid == *pid,
            // TODO: when ProcessMeta gains a pgid field, compare child.pgid
            // against `*pgid` here.
            WaitPid::Pgid(_pgid) => {
                // No child currently carries a process‑group ID, so this
                // filter never matches.  When pgid support is added, change
                // this to `child.pgid == *pgid`.
                false
            }
        }
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
    // Because this kernel does not yet implement proper process‑group
    // tracking (getpgid / setpgid / setsid are stubs), pid == 0 is treated
    // identically to pid == -1.  pid < -1 uses the Pgid filter which
    // currently never matches, so callers passing a negative value other
    // than -1 will receive ECHILD.
    let wait_pid = match pid {
        ..=-2 => WaitPid::Pgid((-pid) as u32),
        -1 | 0 => WaitPid::Any,
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

        let pair = children
            .iter()
            .find(|child| child.all_tasks_exited())
            .map(|child| Arc::clone(child));

        drop(children);

        if let Some(child) = pair {
            let found_pid = child.pid;
            let exit_code = child.inner_lock().get_locked_sigtable().exit_code();
            let child_usage = child.meta_lock().usage;

            if !wstatus.is_null() {
                let proc_inner = task.process.inner_lock();
                let memory_set = proc_inner.get_locked_memory_set_read();
                let value = if exit_code >= 128 && exit_code <= 255 {
                    exit_code
                } else {
                    exit_code << 8
                };
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
