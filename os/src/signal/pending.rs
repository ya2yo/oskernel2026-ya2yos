//! pending signal 选择与默认动作分发。
//!
//! 本模块负责检查当前任务是否有未屏蔽 pending signal，并在 trap 返回前
//! 决定是构造用户 handler frame，还是执行 ignore/stop/continue/terminate
//! 等默认动作。

use log::debug;

use super::{send_signal_to_thread_group, setup_frame, SigActionFlags, SigOp, SigSet, SIGCHLD};
use crate::task::{current_task, exit_current_and_run_next, stop_current_and_run_next, Process};

pub fn check_if_any_sig_for_current_task() -> Option<usize> {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();

    task_inner
        .sig_pending
        .difference(task_inner.sig_mask)
        .peek_front()
}

/// 消费当前任务最低编号的、不会中断阻塞 syscall 的 pending signal。
///
/// 默认忽略（例如 `SIGCHLD`）或显式 `SIG_IGN` 的信号不能让 `read(2)`、
/// `write(2)` 等阻塞操作错误返回 `EINTR`。调用方消费后应重新检查等待条件；
/// 可见信号则保留给 trap return 建立用户 handler frame。
pub fn consume_ignorable_pending_signal_for_current_task() -> bool {
    let task = current_task().unwrap();
    let Some(signo) = ({
        let task_inner = task.inner_lock();
        task_inner
            .sig_pending
            .difference(task_inner.sig_mask)
            .peek_front()
    }) else {
        return false;
    };
    let signal = SigSet::from_sig(signo);
    let sig_action = task
        .process
        .with_sigtable(|sigtable| sigtable.action(signo));
    let ignorable = sig_action.is_ignored()
        || (!sig_action.is_handler() && signal.default_op() == SigOp::Ignore);
    if !ignorable {
        return false;
    }

    let mut task_inner = task.inner_lock();
    if !task_inner.sig_pending.contains(signal) {
        return false;
    }
    task_inner.sig_pending.remove(signal);
    task_inner.sig_pending_info[signo] = None;
    true
}

pub fn handle_signal(signo: usize) {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    let signal = SigSet::from_sig(signo);
    debug!(
        "[handle_signal] signo={},handle signal {:?}, sepc={:#x}",
        signo,
        signal,
        task_inner.trap_cx().get_sepc()
    );
    let sig_action = task
        .process
        .with_sigtable(|sigtable| sigtable.action(signo));
    task_inner.sig_pending.remove(signal);
    let siginfo = task_inner.sig_pending_info[signo].take();
    drop(task_inner);
    drop(task);
    if sig_action.is_handler() {
        // debug!("handle_signal: setup_frame!");
        setup_frame(signo, sig_action, siginfo);
        // 标记信号已拦截：可中断 syscall 应返回 EINTR
        let task = current_task().unwrap();
        task.inner_lock().sig_eintr = true;
    } else {
        let default_op = SigSet::from_sig(signo).default_op();
        if sig_action.is_ignored() {
            debug!("handle_signal: ignore (SIG_IGN), signo={}", signo);
            return;
        }
        match default_op {
            SigOp::Ignore => {
                debug!("handle_signal: ignore (SIG_IGN), signo={}", signo);
            }
            SigOp::Stop => {
                debug!("handle_signal: stop, signo={}", signo);
                let task = current_task().unwrap();
                let parent_pid = task.ppid();
                task.process.meta_lock().stopped_signal = Some(signo);
                if let Some(parent) = Process::get_process_arc_by_pid(parent_pid) {
                    parent.meta_lock().child_exit_event.wake();
                    let no_cld_stop = parent.with_sigtable(|sigtable| {
                        sigtable
                            .action(SIGCHLD)
                            .act
                            .sa_flags
                            .contains(SigActionFlags::SA_NOCLDSTOP)
                    });
                    if !no_cld_stop {
                        let _ = send_signal_to_thread_group(parent_pid, SigSet::SIGCHLD);
                    }
                }
                drop(task);
                stop_current_and_run_next();
            }
            SigOp::Continue => {
                debug!("handle_signal: continue, signo={}", signo);
                current_task().unwrap().process.meta_lock().stopped_signal = None;
            }
            op @ (SigOp::Terminate | SigOp::CoreDump) => {
                debug!("handle_signal: terminate, signo={}", signo);
                current_task()
                    .unwrap()
                    .process
                    .meta_lock()
                    .termination_signal = Some((signo, op == SigOp::CoreDump));
                exit_current_and_run_next((signo + 128) as i32);
            }
        }
    }
}
