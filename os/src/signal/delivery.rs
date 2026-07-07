//! signal 投递路径。
//!
//! 本模块负责把 signal 加入目标 task/thread group 的 pending 集合、唤醒
//! 可中断等待，以及实现用户态 `kill(2)` 可见的 uid/session 权限检查。
//! 内核内部投递 helper 不做权限检查，调用方需要按路径选择合适接口。

use alloc::collections::BTreeSet;

use super::{SigOp, SigSet, SIGCHLD, SIGCONT, SIGKILL, SIG_IGN};
use crate::{
    task::{current_task, ready_queue, tid_to_task, Process, TaskControlBlock, TaskStatus},
    utils::SysErrNo,
};

#[derive(Clone, Copy)]
struct SignalCred {
    real_uid: u32,
    effective_uid: u32,
    saved_uid: u32,
    sid: usize,
}

/// 向 task 挂起信号，并按信号语义把可唤醒的任务放回 ready queue。
///
/// 返回值表示本次投递是否把 `Stopped` 任务恢复为 `Ready`。`kill(SIGCONT)`
/// 需要用这个结果决定是否主动调度一次，让刚恢复的子进程先处理 SIGCONT。
pub(super) fn add_signal(task: &TaskControlBlock, signal: SigSet) -> bool {
    let mut task_inner = task.inner_lock();
    // debug!("add signal: tid {}, signal: {}", task.tid(), signal.bits());
    task_inner.sig_pending |= signal;
    let interrupt_wait = signal.peek_front().is_some_and(|signo| {
        if signo == SIGCHLD {
            return task
                .process
                .with_sigtable(|sigtable| sigtable.action(SIGCHLD).customed);
        }
        task.process.with_sigtable(|sigtable| {
            let action = sigtable.action(signo);
            action.act.sa_handler != SIG_IGN
                && (action.customed || SigSet::from_sig(signo).default_op() != SigOp::Ignore)
        })
    });
    if task_inner.task_status == TaskStatus::Stopped
        && signal.intersects(SigSet::SIGCONT | SigSet::SIGKILL)
    {
        task_inner.task_status = TaskStatus::Ready;
        drop(task_inner);
        if let Some(task) = tid_to_task::tid2task(task.tid()) {
            ready_queue::add_task(&task);
        }
        return true;
    }
    if task_inner.task_status == TaskStatus::Blocked {
        // 可中断等待必须通过 interrupt_waker 唤醒，设置 interrupted 标志；
        // 否则 block_on 会重新 poll 并继续睡眠，SIGTERM 等终止信号无法
        // 打断 accept/recv/futex 等阻塞 syscall。默认 SIGCHLD 等可忽略信号
        // 只唤醒任务，由 waitpid/select 等 syscall 内部按语义继续等待。
        drop(task_inner);
        if !(interrupt_wait && task.wake_interruptible()) {
            let mut task_inner = task.inner_lock();
            if task_inner.task_status == TaskStatus::Blocked {
                task_inner.task_status = TaskStatus::Ready;
                drop(task_inner);
                if let Some(task) = tid_to_task::tid2task(task.tid()) {
                    ready_queue::add_task(&task);
                }
            }
        }
    }
    false
}

fn signal_cred_from_task(task: &TaskControlBlock) -> SignalCred {
    let sid = task.process.sid();
    let inner = task.inner_lock();
    SignalCred {
        real_uid: inner.user_id as u32,
        effective_uid: inner.effective_uid,
        saved_uid: inner.saved_uid,
        sid,
    }
}

fn signal_cred_from_process(proc: &Process) -> Option<SignalCred> {
    let (sid, tasks) = {
        let meta = proc.meta_lock();
        (meta.sid, meta.tasks.clone())
    };
    let task = tasks.iter().find_map(|task| task.upgrade())?;
    let inner = task.inner_lock();
    Some(SignalCred {
        real_uid: inner.user_id as u32,
        effective_uid: inner.effective_uid,
        saved_uid: inner.saved_uid,
        sid,
    })
}

fn can_send_signal(sender: SignalCred, target: SignalCred, signo: usize) -> bool {
    if sender.effective_uid == 0 {
        return true;
    }
    if signo == SIGCONT && sender.sid == target.sid {
        return true;
    }
    sender.real_uid == target.real_uid
        || sender.real_uid == target.saved_uid
        || sender.effective_uid == target.real_uid
        || sender.effective_uid == target.saved_uid
}

fn deliver_signal_to_thread_group(proc: &Process, sig: SigSet) -> usize {
    if sig.is_empty() {
        return 0;
    }

    // debug!("{} receive signal, my parent is {}", proc.pid, proc.ppid());
    let group_exiting = proc.is_group_exiting();
    if !group_exiting {
        if let Some(signo) = sig.peek_front() {
            match SigSet::from_sig(signo).default_op() {
                SigOp::Terminate => proc.meta_lock().termination_signal = Some((signo, false)),
                SigOp::CoreDump => proc.meta_lock().termination_signal = Some((signo, true)),
                _ => {}
            }
        }
    }
    let tasks = proc.meta_lock().tasks.clone();
    let mut resumed = 0;
    for task in tasks.iter() {
        if let Some(task) = task.upgrade() {
            if add_signal(&task, sig) {
                resumed += 1;
            }
        }
    }
    if resumed > 0 && sig.contains(SigSet::SIGCONT) {
        proc.meta_lock().continued_signal = Some(SIGCONT);
        if let Some(parent) = Process::get_process_arc_by_pid(proc.ppid()) {
            parent.meta_lock().child_exit_event.wake();
        }
    }
    resumed
}

fn send_permitted_signal_to_process(
    sender: SignalCred,
    proc: &Process,
    sig: SigSet,
    signo: usize,
) -> Result<usize, SysErrNo> {
    let target = signal_cred_from_process(proc).ok_or(SysErrNo::ESRCH)?;
    if !can_send_signal(sender, target, signo) {
        return Err(SysErrNo::EPERM);
    }
    Ok(deliver_signal_to_thread_group(proc, sig))
}

fn current_signal_cred() -> Result<SignalCred, SysErrNo> {
    let current = current_task().ok_or(SysErrNo::ESRCH)?;
    Ok(signal_cred_from_task(&current))
}

/// `kill(2)` 路径：按 Linux 权限规则向单个进程发送信号。
pub fn send_user_signal_to_thread_group(
    pid: usize,
    sig: SigSet,
    signo: usize,
) -> Result<usize, SysErrNo> {
    let sender = current_signal_cred()?;
    let proc = Process::get_process_arc_by_pid(pid).ok_or(SysErrNo::ESRCH)?;
    send_permitted_signal_to_process(sender, &proc, sig, signo)
}

/// `kill(2)` 路径：按 Linux 权限规则向整个进程组发送信号。
pub fn send_user_signal_to_process_group(
    pgid: usize,
    sig: SigSet,
    signo: usize,
) -> Result<usize, SysErrNo> {
    let sender = current_signal_cred()?;
    let mut seen = BTreeSet::new();
    let mut found = false;
    let mut permitted = false;
    let mut resumed = 0;

    for (_, task) in tid_to_task::get_all_tasks() {
        let proc = &task.process;
        if proc.pgid() != pgid || !seen.insert(proc.pid) {
            continue;
        }
        found = true;
        match send_permitted_signal_to_process(sender, proc, sig, signo) {
            Ok(count) => {
                permitted = true;
                resumed += count;
            }
            Err(SysErrNo::EPERM) => {}
            Err(err) => return Err(err),
        }
    }

    if !found {
        Err(SysErrNo::ESRCH)
    } else if !permitted {
        Err(SysErrNo::EPERM)
    } else {
        Ok(resumed)
    }
}

/// `kill(-1, sig)` 路径：向除 init 和自身之外的可访问进程发送信号。
pub fn send_user_signal_to_accessible_processes(
    sig: SigSet,
    signo: usize,
) -> Result<usize, SysErrNo> {
    let current = current_task().ok_or(SysErrNo::ESRCH)?;
    let sender = signal_cred_from_task(&current);
    let self_pid = current.pid();
    drop(current);

    let mut seen = BTreeSet::new();
    let mut found = false;
    let mut permitted = false;
    let mut resumed = 0;

    for (_, task) in tid_to_task::get_all_tasks() {
        let proc = &task.process;
        if proc.pid == 1 || proc.pid == self_pid || !seen.insert(proc.pid) {
            continue;
        }
        found = true;
        match send_permitted_signal_to_process(sender, proc, sig, signo) {
            Ok(count) => {
                permitted = true;
                resumed += count;
            }
            Err(SysErrNo::EPERM) => {}
            Err(err) => return Err(err),
        }
    }

    if !found {
        Err(SysErrNo::ESRCH)
    } else if !permitted {
        Err(SysErrNo::EPERM)
    } else {
        Ok(resumed)
    }
}

/// 向进程/线程组发信号。
///
/// 成功时返回被本次信号从 `Stopped` 唤醒的线程数；syscall 层仍需把
/// `kill(2)` 的用户可见返回值规整为 0。
pub fn send_signal_to_thread_group(pid: usize, sig: SigSet) -> Result<usize, SysErrNo> {
    let proc = Process::get_process_arc_by_pid(pid).ok_or(SysErrNo::ESRCH)?;
    Ok(deliver_signal_to_thread_group(&proc, sig))
}

pub fn send_signal_to_thread(tid: usize, sig: SigSet) {
    if let Some(task) = tid_to_task::tid2task(tid) {
        add_signal(&task, sig);
    }
}

pub fn send_signal_to_thread_of_proc(pid: usize, tid: usize, sig: SigSet) {
    if let Some(task) = tid_to_task::tid2task(tid) {
        if task.pid() == pid {
            add_signal(&task, sig);
        }
    }
}

pub fn send_signal_to_process_group(pgid: usize, sig: SigSet) {
    let mut sent = BTreeSet::new();
    for (_, task) in tid_to_task::get_all_tasks() {
        if task.process.pgid() == pgid && sent.insert(task.pid()) {
            let _ = send_signal_to_thread_group(task.pid(), sig);
        }
    }
}

/// 向除自身以及 `init` 进程之外的所有进程发送信号。
///
/// 总是返回 Ok(0)。调用者负责向该函数提供 self tid。
pub fn send_access_signal(self_tid: usize, sig: SigSet) -> Result<usize, SysErrNo> {
    let all_tasks = tid_to_task::get_all_tasks();
    for (tid, task) in all_tasks {
        if tid != self_tid && task.pid() != 1 {
            add_signal(&task, sig);
        }
    }
    Ok(0)
}
