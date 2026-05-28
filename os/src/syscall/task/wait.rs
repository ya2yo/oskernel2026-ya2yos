use core::{future::poll_fn, task::Poll};

use alloc::{
    sync::Arc,
    vec::Vec,
};
use log::debug;

use crate::{
    mm::put_data, signal::{SIG_IGN, SIGCHLD, SigActionFlags, SigOp, SigSet, check_if_any_sig_for_current_task}, syscall::options::WaitOption, task::{Process, block_on, current_task, interruptible, suspend_current_and_run_next}, utils::{SysErrNo, SyscallRet}
};

#[derive(Debug, Clone, Copy)]
enum WaitPid {
    /// Wait for any child process
    Any,
    /// Wait for the child whose process ID is equal to the value.
    Pid(usize),
}

impl WaitPid {
    fn apply(&self, child: &Process) -> bool {
        match self {
            WaitPid::Any => true,
            WaitPid::Pid(pid) => child.pid == *pid,
        }
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/waitpid.2.html
pub fn sys_waitpid(pid: i32, wstatus: *mut i32, options: u32) -> SyscallRet {
    let options = WaitOption::from_bits_truncate(options);
    debug!("sys_waitpid <= pid: {pid:?}, options: {options:?}");

    if pid < -1 {
        panic!("[sys_waitpid] pgid not supported: pid={}", pid);
    }

    // pid=0 means any child in the same process group. Since we treat all
    // processes as belonging to the same group, map 0 to Any just like -1.
    let wait_pid = if pid <= 0 {
        WaitPid::Any
    } else {
        WaitPid::Pid(pid as _)
    };

    block_on(interruptible(poll_fn(|cx| {
        let task = current_task().unwrap();
        let mut process_meta = task.process.meta_lock();

        let children: Vec<Arc<Process>> = process_meta
            .children
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|child| wait_pid.apply(child))
            .collect();

        if children.is_empty() {
            return Poll::Ready(Err(SysErrNo::ECHILD));
        }

        let pair = children
            .iter()
            .enumerate()
            .find(|(_, child)| child.all_tasks_exited())
            .map(|(idx, child)| (idx, Arc::clone(child)));

        drop(children);

        if let Some((idx, child)) = pair {
            let found_pid = child.pid;
            let exit_code = child.inner_lock().get_locked_sigtable().exit_code();

            if !wstatus.is_null() {
                let token = task
                    .process
                    .inner_lock()
                    .get_locked_memory_set_read()
                    .token();
                if exit_code >= 128 && exit_code <= 255 {
                    put_data(token, wstatus, exit_code);
                } else {
                    put_data(token, wstatus, exit_code << 8);
                }
            }

            if !options.contains(WaitOption::WNOWAIT) {
                process_meta.children.remove(idx);
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
                    || (!act.customed
                        && SigSet::from_sig(signo).default_op() == SigOp::Ignore);
                if ignorable {
                    task.inner_lock().sig_pending.remove(SigSet::from_sig(signo));
                    drop(task);
                    return Poll::Pending;
                }
                if act.customed {
                    if !act.act.sa_flags.contains(SigActionFlags::SA_RESTART) {
                        drop(task);
                        return Poll::Ready(Err(SysErrNo::EINTR));
                    }
                    task.inner_lock().sig_pending.remove(SigSet::from_sig(signo));
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
