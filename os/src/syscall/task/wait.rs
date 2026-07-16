//! Linux wait-family syscall implementation.
//!
//! 本文件同时承载 `wait4`/`waitpid` 和 `waitid` 的核心逻辑。两类 syscall
//! 都做同一件事：在当前进程的 children 列表中查找符合 selector/options
//! 的子进程状态变化；如果已经有可返回事件则立即回写用户缓冲并返回，
//! 否则根据 `WNOHANG`、pending signal 与 child-exit event 决定返回或阻塞。
//!
//! 这里故意不使用通用的 `task::interruptible()` 包装阻塞等待。wait 系列
//! syscall 必须先查看 pending signal 的 disposition，才能区分：
//!
//! - 默认忽略或显式忽略的信号：消费后继续等；
//! - 自定义 handler 且没有 `SA_RESTART`：返回 `EINTR`；
//! - 自定义 handler 且带 `SA_RESTART`：返回内部 `ERESTART`，由 signal frame
//!   逻辑在 `sigreturn` 后重启原 syscall。
//!
//! 如果套用 `interruptible()`，`wake_interruptible()` 设置的内部 interrupted
//! 标志会在上述判断前被直接转换成 `EINTR`，从而破坏 `SA_RESTART` 语义。

use core::{future::poll_fn, task::Poll};

use alloc::{sync::Arc, vec::Vec};
use linux_raw_sys::general::{
    CLD_CONTINUED, CLD_DUMPED, CLD_EXITED, CLD_KILLED, CLD_STOPPED, P_ALL, P_PGID, P_PID, P_PIDFD,
};
use log::debug;

use crate::{
    mm::copy_to_user,
    signal::{check_if_any_sig_for_current_task, SigActionFlags, SigInfo, SigOp, SigSet, SIGCHLD},
    syscall::options::WaitOption,
    task::{block_on, current_task, Process, TaskControlBlock},
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

/// 将 Ya2yOS 内部保存的退出信息编码成 Linux `waitpid()` 写入 `wstatus`
/// 的整数格式。
///
/// - 普通 `exit(code)`：高 8 位保存退出码，即 `code << 8`。
/// - 信号终止：低 7 位保存终止信号；如果产生 core dump，再置位 `0x80`。
///
/// `waitid()` 不使用这个编码，它通过 `siginfo_t.si_code/si_status` 返回
/// 更结构化的状态信息。
fn wait_status_from_exit_code(exit_code: i32, termination_signal: Option<(usize, bool)>) -> i32 {
    if let Some((signo, dumped_core)) = termination_signal {
        signo as i32 | if dumped_core { 0x80 } else { 0 }
    } else {
        exit_code << 8
    }
}

/// 在 wait 即将阻塞前处理当前任务的一个 pending signal。
///
/// 返回值表示 wait syscall 应该立即返回给 trap 层的错误：
///
/// - `None`：该信号对 wait 可忽略，已经从 pending 集中移除，调用方应继续等待；
/// - `Some(EINTR)`：信号有用户可见效果，但 handler 不带 `SA_RESTART`；
/// - `Some(ERESTART)`：信号 handler 带 `SA_RESTART`，调用方把内部错误交给
///   `setup_frame()`，由信号返回路径重启原 syscall。
///
/// 注意：`ERESTART` 不是用户态可见 errno。它只在内核 trap/signal 路径内部
/// 使用。这里也不能移除需要交付给用户 handler 的 pending signal，否则
/// `trap_return()` 就没有机会建立用户态信号帧。
fn wait_pending_signal_errno(task: &TaskControlBlock, signo: usize) -> Option<SysErrNo> {
    let signal = SigSet::from_sig(signo);
    let sig_action = task
        .process
        .with_sigtable(|sigtable| sigtable.action(signo));
    let ignorable = signo == SIGCHLD
        || sig_action.is_ignored()
        || (!sig_action.is_handler() && signal.default_op() == SigOp::Ignore);
    if ignorable {
        task.inner_lock().sig_pending.remove(signal);
        None
    } else if sig_action.act.sa_flags.contains(SigActionFlags::SA_RESTART) {
        Some(SysErrNo::ERESTART)
    } else {
        Some(SysErrNo::EINTR)
    }
}

/// 实现 Linux `wait4(2)` / `waitpid(2)` 的主体。
///
/// 本内核 syscall 分发表把 Linux `wait4(pid, wstatus, options, rusage)` 的前
/// 三个参数转到这里；当前没有实现用户态 `rusage` 回写，但会把已回收子进程的
/// usage 累计到父进程的 `RUSAGE_CHILDREN` 统计中。
///
/// selector 语义：
///
/// - `pid > 0`：等待指定 PID 的 child；
/// - `pid == 0`：等待当前进程组内任意 child；
/// - `pid == -1`：等待任意 child；
/// - `pid < -1`：等待进程组 ID 为 `-pid` 的 child；
/// - `pid == i32::MIN`：`-pid` 会溢出，按 Linux 兼容语义返回 `ESRCH`。
///
/// options 语义：
///
/// - 默认只等待 `exit_signal == SIGCHLD` 的普通 fork child；
/// - `__WCLONE` 只等待 clone child；
/// - `__WALL` 同时等待普通 child 和 clone child；
/// - `WNOHANG` 在没有可返回事件时返回 `0`；
/// - `WNOWAIT` 只观察状态，不从 children 列表中回收 zombie；
/// - `WUNTRACED/WSTOPPED` 允许返回 stopped child 状态。
///
/// 成功返回：
///
/// - 返回发生状态变化的 child pid；
/// - `wstatus != NULL` 时写入 Linux wait status 编码；
/// - 对退出事件，除 `WNOWAIT` 外会移除 children 引用并从全局 pid map 回收。
///
/// 阻塞行为参考 https://man7.org/linux/man-pages/man2/waitpid.2.html。
pub fn sys_waitpid(pid: i32, wstatus: *mut i32, options: u32) -> SyscallRet {
    // Unknown waitpid(2) option bits must be rejected before looking for
    // children; otherwise truncation can turn an invalid request into a
    // misleading ECHILD result.
    let options = WaitOption::from_bits(options).ok_or(SysErrNo::EINVAL)?;
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

    block_on(poll_fn(|cx| {
        let task = current_task().unwrap();
        let mut process_meta = task.process.meta_lock();

        // Debug-only snapshot: children 中可能有已经 drop 的 Weak，也可能有
        // clone child。这里把 exit_signal 和 all_tasks_exited 打出来，便于
        // 排查 ECHILD / wait 过滤条件不符合预期的问题。
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

        // 第一步只筛 selector/options，得到“本次 syscall 有资格等待”的 child。
        // 如果这里为空，说明调用者没有符合条件的 child，Linux 语义是 ECHILD；
        // 不是 WNOHANG 返回 0。
        if children.is_empty() {
            return Poll::Ready(Err(SysErrNo::ECHILD));
        }

        // 优先返回 stopped 状态。当前内核只记录一个 stopped_signal，
        // 返回后清掉该事件，避免下一次 wait 重复报告同一个 stop。
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

        // 退出事件要求 child 的所有 task 都已经退出。这里暂不处理 continued
        // 状态；`waitid()` 下方有单独的 WCONTINUED 分支。
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
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
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
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
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
                // 回收 zombie 时，把子进程及其 descendants 的资源使用量累计
                // 到当前进程，供后续 getrusage(RUSAGE_CHILDREN) 查询。
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
            // Check for pending signals before going to sleep. wait4/waitpid
            // need syscall-restart semantics, so they cannot use the generic
            // task::interruptible() wrapper: the wrapper would turn the wakeup
            // into EINTR before wait can inspect SA_RESTART.
            if let Some(signo) = check_if_any_sig_for_current_task() {
                drop(process_meta);
                if let Some(errno) = wait_pending_signal_errno(&task, signo) {
                    drop(task);
                    return Poll::Ready(Err(errno));
                }
                drop(task);
                return Poll::Pending;
            }

            // 注册在父进程级 child_exit_event 上。子进程 exit/stop/continue
            // 时会 wake 这个 event，使 block_on 重新 poll 一轮 children 状态。
            process_meta.child_exit_event.register(cx.waker());
            drop(process_meta);
            drop(task);
            Poll::Pending
        }
    }))
}

/// 实现 Linux `waitid(2)`。
///
/// `waitid()` 和 `waitpid()` 共享 child selector 与 clone 过滤逻辑，但用户可见
/// ABI 不同：
///
/// - `options` 必须至少包含 `WEXITED`、`WSTOPPED`、`WCONTINUED` 之一；
/// - 成功时通常返回 `0`，具体 child pid/status 写入 `siginfo_t`；
/// - `infop == NULL` 时本实现为了兼容现有调用点返回 child pid；
/// - `WNOHANG` 且没有可返回事件时返回 `0`，并把 `infop` 清零；
/// - `WNOWAIT` 只观察事件，不消费 stopped/continued 状态，也不回收 zombie。
///
/// `idtype/id` selector：
///
/// - `P_ALL`：等待任意 child；
/// - `P_PID`：等待指定 pid，`id <= 0` 为 `EINVAL`；
/// - `P_PGID`：`id == 0` 表示当前进程组，`id > 0` 表示指定进程组；
/// - `P_PIDFD`：当前尚未实现，返回 `EBADF`。
///
/// 阻塞与信号打断语义和 `sys_waitpid()` 保持一致。
/// 参考 https://www.man7.org/linux/man-pages/man2/wait.2.html。
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

    block_on(poll_fn(|cx| {
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
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
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
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
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
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
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
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
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
                if let Some(errno) = wait_pending_signal_errno(&task, signo) {
                    drop(task);
                    return Poll::Ready(Err(errno));
                }
                drop(task);
                return Poll::Pending;
            }

            process_meta.child_exit_event.register(cx.waker());
            drop(process_meta);
            drop(task);
            Poll::Pending
        }
    }))
}
