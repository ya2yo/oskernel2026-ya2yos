pub mod sigact;
pub mod signal;

use core::mem::size_of;

use alloc::{collections::BTreeSet, slice, sync::Arc};
use linux_raw_sys::general::SIGEV_MAX_SIZE;
use log::{debug, error, warn};
pub use sigact::*;
pub use signal::*;
use spin::Lazy;

use crate::{
    arch::{
        context::{MachineContext, UserContext},
        memory_layout::{self, USER_STACK_SIZE},
        trap_interface::get_trap_cause,
    },
    mm::{copy_from_user_val, copy_to_user, copy_to_user_val, VirtAddr},
    task::{
        current_task, exit_current_and_run_next, ready_queue, stop_current_and_run_next,
        tid_to_task, Process, TaskControlBlock, TaskStatus,
    },
    timer::TimeVal,
    trap::trap_types::{Exception, Trap},
    utils::{SysErrNo, SyscallRet},
};

pub const SIG_MAX_NUM: usize = SIGEV_MAX_SIZE as usize;
pub const SIG_ERR: usize = usize::MAX;
pub const SIG_DFL: usize = 0;
pub const SIG_IGN: usize = 1;

static PSELECT_ITIMER_WAITERS: Lazy<kspin::SpinNoIrq<BTreeSet<usize>>> =
    Lazy::new(|| kspin::SpinNoIrq::new(BTreeSet::new()));

pub struct PselectItimerGuard {
    tid: usize,
}

impl Drop for PselectItimerGuard {
    fn drop(&mut self) {
        PSELECT_ITIMER_WAITERS.lock().remove(&self.tid);
    }
}

extern "C" {
    pub fn sigreturn_trampoline();
}

pub fn check_if_any_sig_for_current_task() -> Option<usize> {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();

    task_inner
        .sig_pending
        .difference(task_inner.sig_mask)
        .peek_front()
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
        .inner_lock()
        .get_locked_sigtable()
        .action(signo);
    task_inner.sig_pending.remove(signal);
    drop(task_inner);
    drop(task);
    if sig_action.customed {
        // debug!("handle_signal: setup_frame!");
        setup_frame(signo, sig_action);
        // 标记信号已拦截：可中断 syscall 应返回 EINTR
        let task = current_task().unwrap();
        task.inner_lock().sig_eintr = true;
    } else {
        match SigSet::from_sig(signo).default_op() {
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
                    let no_cld_stop = parent
                        .inner_lock()
                        .get_locked_sigtable()
                        .action(SIGCHLD)
                        .act
                        .sa_flags
                        .contains(SigActionFlags::SA_NOCLDSTOP);
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
/// 在用户态栈空间构建一个 Frame
/// 构建这个帧的目的就是为了执行完信号处理程序后返回到内核态，
/// 并恢复原来内核栈的内容
pub fn setup_frame(signo: usize, sig_action: KSigAction) {
    // debug!("customed sa_handler={:#x}", sig_action.act.sa_handler);
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    // SA_RESETHAND: 在调用信号处理函数之前将 handler 重置为 SIG_DFL
    // 这样信号处理函数仅在第一次收到信号时被调用
    if sig_action
        .act
        .sa_flags
        .contains(SigActionFlags::SA_RESETHAND)
    {
        proc_inner
            .get_locked_sigtable()
            .set_action(signo, KSigAction::new(signo, false));
    }

    let mut task_inner = task.inner_lock();
    let trap_cx = task_inner.trap_cx();
    let mut user_sp = trap_cx.get_sp();

    // 动态查找包含当前 sp 的 MapArea，以此确定栈的真实边界。
    // 这对 mmap 分配的线程栈也能正确工作。
    let memory_set = proc_inner.get_locked_memory_set_read();
    let sp_vpn = VirtAddr::from(user_sp).floor();
    let stack_bottom = memory_set
        .get_ref()
        .areas
        .iter()
        .find(|area| area.vpn_range.start() <= sp_vpn && sp_vpn < area.vpn_range.end())
        .map(|area| VirtAddr::from(area.vpn_range.start()).0)
        .unwrap_or_else(|| user_sp.saturating_sub(USER_STACK_SIZE));

    let min_frame_size = if sig_action.act.sa_flags.contains(SigActionFlags::SA_SIGINFO) {
        size_of::<UserContext>() + size_of::<SigInfo>() + size_of::<usize>() // uctx + siginfo + magic
    } else {
        size_of::<MachineContext>() + size_of::<SigSet>() + size_of::<usize>() // mctx + mask + magic
    };
    if user_sp < stack_bottom + min_frame_size {
        // 栈空间不足，无法安全设置信号帧，直接终止进程
        warn!(
            "setup_frame: user stack too small for signal {}, sp={:#x}, stack_bottom={:#x}",
            signo, user_sp, stack_bottom
        );
        drop(task_inner);
        drop(memory_set);
        drop(proc_inner);
        drop(task);
        exit_current_and_run_next((signo + 128) as i32);
    } else {
        // if this syscall wants to restart
        if get_trap_cause() == Trap::Exception(Exception::Syscall)
            && trap_cx.get_a0() == SysErrNo::ERESTART as usize
        {
            // 我们的内核是不可抢占的，因此理论上这不会发生
            warn!("SysErrNo::ERESTART should not happen: this kernel is non-preemptive!");
            // and if `SA_RESTART` is set
            if sig_action.act.sa_flags.contains(SigActionFlags::SA_RESTART) {
                // debug!("[do_signal] syscall will restart after sigreturn");
                // back to `ecall`
                trap_cx.sepc_step(-4);
                // restore syscall parameter `a0`
                trap_cx.set_a0(trap_cx.origin_a0);
            } else {
                // debug!("[do_signal] syscall was interrupted");
                // will return EINTR after sigreturn
                trap_cx.set_a0(SysErrNo::EINTR as usize);
            }
        }

        if !sig_action.act.sa_flags.contains(SigActionFlags::SA_SIGINFO) {
            // 处理函数 (*sa_handler)(int);
            // 保存 Trap 上下文
            user_sp = user_sp - size_of::<MachineContext>();
            let mctx = trap_cx.as_mctx();
            let ret = copy_to_user(&memory_set, user_sp as usize, unsafe {
                core::slice::from_raw_parts(
                    &mctx as *const MachineContext as *const _,
                    core::mem::size_of::<MachineContext>(),
                )
            });
            if ret.is_err() {
                error!("[setup_frame] save MachineContext should not cause error!");
                panic!()
            }

            // signal mask
            user_sp = user_sp - size_of::<SigSet>();
            let sigset = task_inner.sig_mask;
            let ret = copy_to_user(&memory_set, user_sp, unsafe {
                core::slice::from_raw_parts(
                    &sigset as *const SigSet as *const _,
                    core::mem::size_of::<SigSet>(),
                )
            });
            if ret.is_err() {
                error!("[setup_frame] save signal mask should not cause error!");
                panic!()
            }

            // 不是 sigInfo
            user_sp = user_sp - size_of::<usize>();
            let ret = copy_to_user(&memory_set, user_sp, &[0u8; core::mem::size_of::<usize>()]);
            if ret.is_err() {
                error!("[setup_frame] save others should not cause error!");
                panic!()
            }
        } else {
            // (*sa_sigaction)(int, siginfo_t *, void *) 第三个参数指向UserContext
            let uctx_addr = user_sp - size_of::<UserContext>();
            let siginfo_addr = uctx_addr - size_of::<SigInfo>();
            let sig_sp = siginfo_addr;
            let sig_size = sig_sp - stack_bottom;
            // debug!("sig_size={:#x}", sig_size);
            // debug!("save: uctx_addr = {:#x}", uctx_addr);
            let uctx = UserContext {
                flags: 0,
                link: 0,
                stack: SignalStack::new(sig_sp, sig_size),
                sigmask: task_inner.sig_mask,
                __pad: [0u8; 128],
                mcontext: trap_cx.as_mctx(),
            };
            let ret = copy_to_user(&memory_set, uctx_addr, unsafe {
                core::slice::from_raw_parts(
                    &uctx as *const UserContext as *const _,
                    core::mem::size_of::<UserContext>(),
                )
            });
            if ret.is_err() {
                error!("[setup_frame] save uctx should not cause error!");
                panic!()
            }
            // a2
            trap_cx.set_a2(uctx_addr);

            copy_to_user_val(
                &*memory_set,
                siginfo_addr as *mut SigInfo,
                &SigInfo::new(signo as u32, 0, (-6 as i32) as u32, task.pid() as u32),
            )
            .unwrap();
            // a1
            trap_cx.set_a1(siginfo_addr);

            user_sp = sig_sp;
            // 是 sigInfo
            user_sp = user_sp - size_of::<usize>();
            copy_to_user_val(&*memory_set, user_sp as *mut usize, &usize::MAX).unwrap();
        }

        // checkout(Magic Num)
        user_sp -= size_of::<usize>();
        copy_to_user_val(&*memory_set, user_sp as *mut usize, &0xdeadbeefusize).unwrap();
        // a0
        trap_cx.set_a0(signo);
        // sp
        trap_cx.set_sp(user_sp);
        // 修改Trap
        trap_cx.set_sepc(sig_action.act.sa_handler);
        // ra
        trap_cx.set_ra(
            if sig_action
                .act
                .sa_flags
                .contains(SigActionFlags::SA_RESTORER)
            {
                sig_action.act.sa_restore
            } else {
                let trampoline: usize;
                cfg_if::cfg_if! {
                    if #[cfg(target_arch = "loongarch64")] {
                        trampoline = memory_layout::sigreturn_va();
                    } else if #[cfg(target_arch = "riscv64")] {
                        trampoline = sigreturn_trampoline as *const() as usize;
                    }
                }
                //warn!("set sigreturn_trampoline={:#x} as ra", trampoline);
                trampoline
            },
        );

        // 默认：在处理函数执行期间阻塞当前信号 + sa_mask 中的信号
        // SA_NODEFER: 不自动阻塞当前信号
        let mut new_mask = sig_action.act.sa_mask;
        if !sig_action.act.sa_flags.contains(SigActionFlags::SA_NODEFER) {
            new_mask |= SigSet::from_sig(signo);
        }
        task_inner.sig_mask |= new_mask;
    }
}
/// 恢复栈帧
pub fn restore_frame() -> SyscallRet {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();

    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    let trap_cx = task_inner.trap_cx();
    let mut user_sp = trap_cx.get_sp();

    let checkout: usize = copy_from_user_val(&*memory_set, user_sp as *const usize).unwrap();
    assert!(checkout == 0xdeadbeef, "restore frame checkout error!");
    user_sp += size_of::<usize>();

    // sigInfo标志位
    let sa_siginfo_flag: usize = copy_from_user_val(&*memory_set, user_sp as *const usize).unwrap();
    let sa_siginfo = sa_siginfo_flag == usize::MAX;
    user_sp += size_of::<usize>();

    if !sa_siginfo {
        // signal mask
        task_inner.sig_mask = copy_from_user_val(&*memory_set, user_sp as *const SigSet).unwrap();
        user_sp += size_of::<SigSet>();
        // Trap cx
        let mctx = copy_from_user_val(&*memory_set, user_sp as *const MachineContext).unwrap();
        trap_cx.copy_from_mctx(mctx);
    } else {
        let uctx_addr = user_sp as usize + size_of::<SigInfo>();
        // debug!("load: uctx_addr = {:#x}", uctx_addr);
        let uctx: UserContext =
            copy_from_user_val(&*memory_set, uctx_addr as *const UserContext).unwrap();
        task_inner.sig_mask = uctx.sigmask;
        let mctx = uctx.mcontext;
        trap_cx.copy_from_mctx(mctx);
    }
    // debug!("[restore_frame!] sepc= {:#x}", trap_cx.get_sepc());
    Ok(trap_cx.get_a0())
}

/// 向 task 挂起信号，并按信号语义把可唤醒的任务放回 ready queue。
///
/// 返回值表示本次投递是否把 `Stopped` 任务恢复为 `Ready`。`kill(SIGCONT)`
/// 需要用这个结果决定是否主动调度一次，让刚恢复的子进程先处理 SIGCONT。
fn add_signal(task: &TaskControlBlock, signal: SigSet) -> bool {
    let mut task_inner = task.inner_lock();
    // debug!("add signal: tid {}, signal: {}", task.tid(), signal.bits());
    task_inner.sig_pending |= signal;
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
        // SIGKILL 等信号必须把任务从 pipe/futex 等等待中唤醒，
        // 这样任务才能回到 trap 返回路径处理 pending signal。
        task_inner.task_status = TaskStatus::Ready;
        drop(task_inner);
        if let Some(task) = tid_to_task::tid2task(task.tid()) {
            ready_queue::add_task(&task);
        }
    }
    false
}

/// 向进程/线程组发信号
/// 如果未找到这个进程/线程组，会 return。
///
/// 成功时返回被本次信号从 `Stopped` 唤醒的线程数；syscall 层仍需把
/// `kill(2)` 的用户可见返回值规整为 0。
pub fn send_signal_to_thread_group(pid: usize, sig: SigSet) -> Result<usize, SysErrNo> {
    let process = Process::get_process_arc_by_pid(pid);
    if let Some(proc) = process {
        // debug!("{} receive signal, my parent is {}", pid, proc.ppid());
        let group_exiting = proc.inner_lock().get_locked_sigtable().is_exited();
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
        return Ok(resumed);
    } else {
        // No such process
        return Err(SysErrNo::ESRCH);
    }
}

pub fn send_signal_to_thread(tid: usize, sig: SigSet) {
    if let Some(task) = tid_to_task::tid2task(tid) {
        add_signal(&task, sig);
    }
}

pub fn enter_pselect_itimer_wait(task: &TaskControlBlock) -> PselectItimerGuard {
    PSELECT_ITIMER_WAITERS.lock().insert(task.tid());
    PselectItimerGuard { tid: task.tid() }
}

fn is_pselect_itimer_waiter(task: &TaskControlBlock) -> bool {
    PSELECT_ITIMER_WAITERS.lock().contains(&task.tid())
}

fn add_itimer_signal(task: &TaskControlBlock) {
    let mut task_inner = task.inner_lock();
    task_inner.sig_pending |= SigSet::SIGALRM;
    let should_ready = task_inner.task_status == TaskStatus::Blocked;
    if should_ready {
        task_inner.task_status = TaskStatus::Ready;
    }
    drop(task_inner);

    if should_ready {
        if let Some(task) = tid_to_task::tid2task(task.tid()) {
            ready_queue::add_task(&task);
        }
    }
}

fn add_blocked_itimer_signal(task: &TaskControlBlock) {
    let task_inner = task.inner_lock();
    let should_wake = task_inner.task_status == TaskStatus::Blocked;
    drop(task_inner);

    if should_wake && task.wake_interruptible() {
        let mut task_inner = task.inner_lock();
        task_inner.sig_pending |= SigSet::SIGALRM;
        if task_inner.task_status == TaskStatus::Blocked {
            task_inner.task_status = TaskStatus::Ready;
            drop(task_inner);
            if let Some(task) = tid_to_task::tid2task(task.tid()) {
                ready_queue::add_task(&task);
            }
        }
    }
}

/// Check a task's interval timer and deliver SIGALRM when it expires.
pub fn deliver_itimer_signal(task: &TaskControlBlock) {
    if is_pselect_itimer_waiter(task) {
        return;
    }

    let timer = {
        let task_inner = task.inner_lock();
        task_inner.timer.clone()
    };
    if timer.take_expired_signal(TimeVal::now()) {
        add_itimer_signal(task);
    }
}

/// Deliver expired ITIMER_REAL only to interruptible blocked waits.
pub fn deliver_blocked_itimer_signal(task: &TaskControlBlock) {
    if !task.has_interruptible_waiter() {
        return;
    }

    let timer = {
        let task_inner = task.inner_lock();
        task_inner.timer.clone()
    };
    if timer.take_expired_signal(TimeVal::now()) {
        add_blocked_itimer_signal(task);
    }
}

pub fn send_signal_to_thread_of_proc(pid: usize, tid: usize, sig: SigSet) {
    if let Some(task) = tid_to_task::tid2task(tid) {
        if task.pid() == pid {
            add_signal(&task, sig);
        }
    }
}

// 目前的进程组只是一个进程的所有子进程的集合
pub fn send_signal_to_process_group(_pid: usize, _sig: SigSet) {
    todo!()
}

/// 向除自身以及 `init` 进程之外的所有进程发送信号
/// 总是返回Ok(0)
/// 调用者负责向该函数提供一个self_tid
pub fn send_access_signal(self_tid: usize, sig: SigSet) -> Result<usize, SysErrNo> {
    let all_tasks = tid_to_task::get_all_tasks();
    for (tid, task) in all_tasks {
        if tid != self_tid {
            add_signal(&task, sig);
        }
    }
    Ok(0)
}
