//! signal 投递路径。
//!
//! 本模块负责把 signal 加入目标 task/thread group 的 pending 集合、唤醒
//! 可中断等待，以及实现用户态 `kill(2)` 可见的 uid/session 权限检查。
//! 内核内部投递 helper 不做权限检查，调用方需要按路径选择合适接口。

use alloc::collections::BTreeSet;

use super::{SigInfo, SigOp, SigSet, SIGCHLD, SIGCONT, SIGKILL};
use crate::{
    task::{current_task, ready_queue, tid_to_task, Process, TaskControlBlock, TaskStatus},
    utils::SysErrNo,
};

/// 信号投递权限检查和发送者信息记录所需的最小凭据快照。
///
/// 该结构从发送任务或目标进程的一个存活线程提取，不替代完整的
/// `TaskControlBlockInner` 凭据。`kill(2)` 路径使用 real/effective/saved UID
/// 与会话 ID 判断是否允许投递；成功的用户态投递则使用 `pid` 和 `real_uid`
/// 构造供 `SA_SIGINFO` handler 观察的 `siginfo_t`。
#[derive(Clone, Copy)]
struct SignalCred {
    /// 进程 ID；作为用户态投递信号时 `siginfo_t.si_pid` 的来源。
    pid: usize,
    /// Real UID；用于 Linux 风格的信号权限比较和 `siginfo_t.si_uid`。
    real_uid: u32,
    /// Effective UID；root 发送者可绕过普通 UID 匹配规则。
    effective_uid: u32,
    /// Saved set-user-ID；目标凭据允许与发送者 real/effective UID 匹配。
    saved_uid: u32,
    /// 会话 ID；同一会话内的 `SIGCONT` 允许绕过普通 UID 匹配。
    sid: usize,
}

/// 向 task 挂起信号，并按信号语义把可唤醒的任务放回 ready queue。
///
/// 返回值表示本次投递是否把 `Stopped` 或 `VforkBlocked` 任务恢复为 `Ready`。
/// `kill(SIGCONT)` 需要用这个结果决定是否主动调度一次，让刚恢复的子进程先处理 SIGCONT。
pub(super) fn add_signal(task: &TaskControlBlock, signal: SigSet) -> bool {
    add_signal_with_info(task, signal, None)
}

/// 向 task 挂起信号，并可携带用户态可见的 `siginfo_t`。
///
/// 标准信号在当前实现中不排队：如果同一个 signal 已经处于 pending 状态，
/// 后续重复投递只保留 pending 位，不覆盖第一次记录的 `siginfo_t`。这样可
/// 保持 `kill(2)`/`tkill(2)`/`tgkill(2)` 的 `SA_SIGINFO` handler 能看到
/// 最初触发该 pending signal 的发送者信息。
pub(super) fn add_signal_with_info(
    task: &TaskControlBlock,
    signal: SigSet,
    siginfo: Option<SigInfo>,
) -> bool {
    // Snapshot the disposition before taking TaskControlBlockInner.  Signal
    // delivery can race with trap-return handling on another hart; taking the
    // signal-table lock while holding the task lock reverses the task lock
    // ordering and can deadlock that path.
    let interrupt_wait = signal.peek_front().is_some_and(|signo| {
        if signo == SIGCHLD {
            return task
                .process
                .with_sigtable(|sigtable| sigtable.action(SIGCHLD).is_handler());
        }
        task.process.with_sigtable(|sigtable| {
            let action = sigtable.action(signo);
            !action.is_ignored()
                && (action.is_handler() || SigSet::from_sig(signo).default_op() != SigOp::Ignore)
        })
    });

    let mut task_inner = task.inner_lock();
    // debug!("add signal: tid {}, signal: {}", task.tid(), signal.bits());
    if let Some(signo) = signal.peek_front() {
        if !task_inner.sig_pending.contains(signal) {
            task_inner.sig_pending_info[signo] = siginfo;
        }
    }
    task_inner.sig_pending |= signal;
    let wake_stopped = task_inner.task_status == TaskStatus::Stopped
        && signal.intersects(SigSet::SIGCONT | SigSet::SIGKILL);
    let wake_vfork_parent =
        task_inner.task_status == TaskStatus::VforkBlocked && signal.intersects(SigSet::SIGKILL);
    if wake_stopped || wake_vfork_parent {
        // A vfork parent normally remains asleep until its child exits or
        // execs. SIGKILL is not deferrable, including while that wait is
        // active, so make it runnable to consume the pending signal.
        if wake_vfork_parent {
            task_inner.vfork_wait_child = 0;
        }
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
        pid: task.pid(),
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
        pid: task.pid(),
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

/// 根据发送者凭证构造用户态信号的 `siginfo_t`。
///
/// `signo == 0` 是 `kill(pid, 0)` 权限/存在性探测，不会真正投递信号，
/// 因此不需要生成 `siginfo_t`。
fn siginfo_from_sender(sender: SignalCred, signo: usize) -> Option<SigInfo> {
    if signo == 0 {
        None
    } else {
        Some(SigInfo::new_user(
            signo as u32,
            sender.pid as u32,
            sender.real_uid,
        ))
    }
}

/// 向一个进程的线程组投递信号。
///
/// `siginfo` 为 `None` 时表示内核内部投递路径，用户态 `SA_SIGINFO`
/// handler 会得到空的 fallback siginfo；用户态 `kill(2)` 等路径会传入
/// 已填好发送者 pid/uid 的 siginfo。
fn deliver_signal_to_thread_group(proc: &Process, sig: SigSet, siginfo: Option<SigInfo>) -> usize {
    if sig.is_empty() {
        return 0;
    }

    // debug!("{} receive signal, my parent is {}", proc.pid, proc.ppid());
    let tasks = proc.meta_lock().tasks.clone();
    let mut resumed = 0;
    for task in tasks.iter() {
        if let Some(task) = task.upgrade() {
            if add_signal_with_info(&task, sig, siginfo) {
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
    Ok(deliver_signal_to_thread_group(
        proc,
        sig,
        siginfo_from_sender(sender, signo),
    ))
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
    Ok(deliver_signal_to_thread_group(&proc, sig, None))
}

pub fn send_signal_to_thread(tid: usize, sig: SigSet) {
    if let Some(task) = tid_to_task::tid2task(tid) {
        add_signal(&task, sig);
    }
}

/// 用户态 `tkill(2)` 路径：向指定 tid 投递信号并记录当前任务为发送者。
///
/// 该 helper 不做权限检查，保持原 `tkill(2)` 简化语义；与内部
/// `send_signal_to_thread()` 的区别是会为 `SA_SIGINFO` 保存发送者 siginfo。
pub fn send_user_signal_to_thread(tid: usize, sig: SigSet, signo: usize) {
    if let Some(task) = tid_to_task::tid2task(tid) {
        let siginfo = current_signal_cred()
            .ok()
            .and_then(|sender| siginfo_from_sender(sender, signo));
        add_signal_with_info(&task, sig, siginfo);
    }
}

/// 用户态 `tgkill(2)` 路径：仅当 tid 属于指定 tgid/pid 时投递信号。
///
/// 和 `send_user_signal_to_thread()` 一样，该路径会保存发送者 siginfo，
/// 供后续 `SA_SIGINFO` handler 或 `rt_sigtimedwait()` 消费。
pub fn send_user_signal_to_thread_of_proc(pid: usize, tid: usize, sig: SigSet, signo: usize) {
    if let Some(task) = tid_to_task::tid2task(tid) {
        if task.pid() == pid {
            let siginfo = current_signal_cred()
                .ok()
                .and_then(|sender| siginfo_from_sender(sender, signo));
            add_signal_with_info(&task, sig, siginfo);
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
