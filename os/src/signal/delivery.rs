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

/// 需要表达信号编号之外语义的内核信号来源。
///
/// `execve()` 使用 `SIGKILL` 仅用于在替换共享地址空间前清理遗留的兄弟
/// 线程。真正的 `SIGKILL` 必须始终优先于该清理请求，并终止整个线程组，
/// 而不能被误认为只是 exec 清理线程的内部操作。
#[derive(Clone, Copy, Eq, PartialEq)]
enum SignalDeliverySource {
    /// 普通信号投递，包括用户态系统调用和内核产生的正常信号。
    Normal,
    /// `execve()` 为拆除同一进程中的其他线程而发出的内部信号。
    ExecTeardown,
}

/// 向 task 挂起信号，并按信号语义把可唤醒的任务放回 ready queue。
///
/// 返回值表示本次投递是否把 `Stopped` 或 `VforkBlocked` 任务恢复为 `Ready`。
/// `kill(SIGCONT)` 需要用这个结果决定是否主动调度一次，让刚恢复的子进程先处理 SIGCONT。
pub(super) fn add_signal(task: &TaskControlBlock, signal: SigSet) -> bool {
    add_signal_with_info(task, signal, None, SignalDeliverySource::Normal)
}

/// 向 task 挂起信号，并可携带用户态可见的 `siginfo_t`。
///
/// 标准信号在当前实现中不排队：如果同一个 signal 已经处于 pending 状态，
/// 后续重复投递只保留 pending 位，不覆盖第一次记录的 `siginfo_t`。这样可
/// 保持 `kill(2)`/`tkill(2)`/`tgkill(2)` 的 `SA_SIGINFO` handler 能看到
/// 最初触发该 pending signal 的发送者信息。
fn add_signal_with_info(
    task: &TaskControlBlock,
    signal: SigSet,
    siginfo: Option<SigInfo>,
    source: SignalDeliverySource,
) -> bool {
    // SIGKILL 不能被捕获或忽略。用户信号投递时就记录进程退出原因，
    // 因为被阻塞的任务可能在陷阱返回分发 handle_signal() 之前，
    // 通过调度器的快速路径退出。execve 线程清理等内部 SIGKILL 不携带
    // siginfo，因此不能影响进程最终对父进程暴露的等待状态。
    if signal.contains(SigSet::SIGKILL) && siginfo.is_some() {
        let mut process_meta = task.process.meta_lock();
        if process_meta.group_exit_code.is_none() {
            process_meta
                .termination_signal
                .get_or_insert((SIGKILL, false));
        }
    }

    // 在获取 TaskControlBlockInner 之前先读取信号处置方式。信号投递可能
    // 与另一个 hart 上的陷阱返回处理并发；若持有任务锁时再获取信号表锁，
    // 就会反转该路径的任务锁顺序，从而造成死锁。
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
    // debug!("添加信号：tid {}，信号：{}", task.tid(), signal.bits());
    if signal.contains(SigSet::SIGKILL) {
        match source {
            SignalDeliverySource::ExecTeardown
                if !task_inner.sig_pending.contains(SigSet::SIGKILL) =>
            {
                task_inner.exec_teardown_kill = true;
            }
            // 与 exec 清理并发的普通 SIGKILL 优先级更高：它具有 Linux
            // 进程终止语义，而不只是删除当前兄弟线程。
            SignalDeliverySource::Normal => task_inner.exec_teardown_kill = false,
            SignalDeliverySource::ExecTeardown => {}
        }
    }
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
        // vfork 父线程通常会一直睡眠，直到子线程退出或执行 exec。
        // 即使 vfork 等待正在进行，SIGKILL 也不能被延迟，因此必须将其
        // 设为可运行，以便及时消费 pending 信号。
        if wake_vfork_parent {
            task_inner.vfork_wait_child = 0;
            #[cfg(feature = "perf")]
            {
                task_inner.vfork_parent_ready_at = crate::arch::time::get_ticks();
            }
        }
        task_inner.task_status = TaskStatus::Ready;
        drop(task_inner);
        #[cfg(feature = "perf")]
        if wake_vfork_parent {
            crate::utils::perf::record_vfork_release_signal();
        }
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

/// 从进程的任意一个存活线程提取信号权限检查所需的凭证。
///
/// 进程级的 UID 通常由线程共享，但本实现中的部分凭证字段存储在线程
/// 内部，因此这里复制进程会话 ID 后，从线程列表中寻找第一个仍然存活
/// 的线程读取这些字段。进程没有存活线程时返回 `None`，调用方应将其
/// 视为目标不存在并返回 `ESRCH`。
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

/// 判断发送者是否有权向目标投递指定信号。
///
/// 有效 UID 为 0 的发送者可以绕过普通 UID 匹配。`SIGCONT` 还允许同一
/// 会话中的发送者投递；其他情况要求发送者的 real/effective UID 至少
/// 与目标的 real/saved UID 之一匹配。这里仅负责权限判断，不检查目标
/// 是否存在，也不实际修改 pending 信号集合。
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

    // debug!("进程 {} 收到信号，其父进程为 {}", proc.pid, proc.ppid());
    let tasks = proc.meta_lock().tasks.clone();
    let mut resumed = 0;
    for task in tasks.iter() {
        if let Some(task) = task.upgrade() {
            if add_signal_with_info(&task, sig, siginfo, SignalDeliverySource::Normal) {
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
///
/// `pid` 按进程 ID 查找目标，实际投递会遍历目标线程组中的存活线程。
/// `signo` 用于权限判断和生成 `siginfo_t`，而 `sig` 是真正写入 pending
/// 集合的位集合；因此 `signo == 0` 只执行存在性与权限检查，不会设置
/// pending 位。
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
///
/// 遍历全局任务表时使用 `seen` 将同一进程的多个线程去重。只要至少有
/// 一个目标进程存在但全部权限检查失败，就返回 `EPERM`；如果进程组中
/// 没有目标，则返回 `ESRCH`。
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
///
/// 同样按进程去重，并忽略单个目标的 `EPERM`，只有全部候选目标都不可
/// 访问时才返回 `EPERM`。这里不向 PID 1 或当前进程发送信号，符合本项
/// 目当前对 `kill(-1, sig)` 的语义实现。
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

/// 标记一个仅用于拆除 `execve` 兄弟线程的内部 `SIGKILL`。
///
/// 信号来源会与 pending 位一起发布，因此并发到达的普通 `SIGKILL` 能够
/// 确定性地覆盖该内部清理语义，不会被误认为只是 exec 清理线程的请求。
pub(crate) fn send_exec_teardown_kill(tid: usize) {
    if let Some(task) = tid_to_task::tid2task(tid) {
        add_signal_with_info(
            &task,
            SigSet::SIGKILL,
            None,
            SignalDeliverySource::ExecTeardown,
        );
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
        add_signal_with_info(&task, sig, siginfo, SignalDeliverySource::Normal);
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
            add_signal_with_info(&task, sig, siginfo, SignalDeliverySource::Normal);
        }
    }
}

/// 向指定进程组中的每个进程投递一次内核内部信号。
///
/// 任务表按线程展开，因此通过 `sent` 按进程 ID 去重。该接口是内核
/// 内部路径，不执行用户态 UID 权限检查，也不向调用方返回目标不存在
/// 或投递失败的错误。
pub fn send_signal_to_process_group(pgid: usize, sig: SigSet) {
    let mut sent = BTreeSet::new();
    for (_, task) in tid_to_task::get_all_tasks() {
        if task.process.pgid() == pgid && sent.insert(task.pid()) {
            let _ = send_signal_to_thread_group(task.pid(), sig);
        }
    }
}
