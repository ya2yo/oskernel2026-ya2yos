pub mod sigact;
pub mod signal;

use core::mem::size_of;

use alloc::sync::Arc;
use log::{debug, warn};
pub use sigact::*;
pub use signal::*;

use crate::{
    arch::{
        context::{MachineContext, UserContext},
        memory_layout::{self, USER_STACK_SIZE},
        trap_interface::get_trap_cause,
    },
    mm::{get_data, put_data},
    task::{current_task, exit_current_and_run_next, tid_to_task, Process, TaskControlBlock},
    trap::trap_types::{Exception, Trap},
    utils::{SysErrNo, SyscallRet},
};

pub const SIG_MAX_NUM: usize = 33;
pub const SIG_ERR: usize = usize::MAX;
pub const SIG_DFL: usize = 0;
pub const SIG_IGN: usize = 1;

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
        debug!("handle_signal: setup_frame!");
        setup_frame(signo, sig_action);
    } else {
        debug!("handle_signal: default exit!");
        debug!("sa_handler:{:#x}", sig_action.act.sa_handler as usize);
        // 就在S模式运行,转换成fn(i32)
        if sig_action.act.sa_handler != 1 {
            if sig_action.act.sa_handler == exit_current_and_run_next as *const() as usize {
                exit_current_and_run_next((signo + 128) as i32);
            }
        }
    }
}
/// 在用户态栈空间构建一个 Frame
/// 构建这个帧的目的就是为了执行完信号处理程序后返回到内核态，
/// 并恢复原来内核栈的内容
pub fn setup_frame(signo: usize, sig_action: KSigAction) {
    debug!("customed sa_handler={:#x}", sig_action.act.sa_handler);

    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    let token = task.process.inner_lock().get_locked_memory_set_read().token();

    let trap_cx = task_inner.trap_cx();
    let mut user_sp = trap_cx.get_sp();

    // if this syscall wants to restart
    if get_trap_cause() == Trap::Exception(Exception::Syscall)
        && trap_cx.get_a0() == SysErrNo::ERESTART as usize
    {
        // 我们的内核是不可抢占的，因此理论上这不会发生
        panic!("SysErrNo::ERESTART should not happen: this kernel is non-preemptive!");
        // and if `SA_RESTART` is set
        if sig_action.act.sa_flags.contains(SigActionFlags::SA_RESTART) {
            debug!("[do_signal] syscall will restart after sigreturn");
            // back to `ecall`
            trap_cx.sepc_step(-4);
            // restore syscall parameter `a0`
            trap_cx.set_a0(trap_cx.origin_a0);
        } else {
            debug!("[do_signal] syscall was interrupted");
            // will return EINTR after sigreturn
            trap_cx.set_a0(SysErrNo::EINTR as usize);
        }
    }

    if !sig_action.act.sa_flags.contains(SigActionFlags::SA_SIGINFO) {
        // 处理函数 (*sa_handler)(int);
        // 保存 Trap 上下文
        user_sp = user_sp - size_of::<MachineContext>();
        put_data(token, user_sp as *mut MachineContext, trap_cx.as_mctx());

        // signal mask
        user_sp = user_sp - size_of::<SigSet>();
        put_data(token, user_sp as *mut SigSet, task_inner.sig_mask);

        // 不是 sigInfo
        user_sp = user_sp - size_of::<usize>();
        put_data(token, user_sp as *mut usize, 0);
    } else {
        // (*sa_sigaction)(int, siginfo_t *, void *) 第三个参数指向UserContext
        let uctx_addr = user_sp - size_of::<UserContext>();
        let siginfo_addr = uctx_addr - size_of::<SigInfo>();
        let sig_sp = siginfo_addr;
        let sig_size = sig_sp - (task_inner.user_stack_top - USER_STACK_SIZE);
        // debug!("sig_size={:#x}", sig_size);
        debug!("save: uctx_addr = {:#x}", uctx_addr);
        put_data(
            token,
            uctx_addr as *mut UserContext,
            UserContext {
                flags: 0,
                link: 0,
                stack: SignalStack::new(sig_sp, sig_size),
                sigmask: task_inner.sig_mask,
                __pad: [0u8; 128],
                mcontext: trap_cx.as_mctx(),
            },
        );
        // a2
        trap_cx.set_a2(uctx_addr);
        // TODO: HXC: 这里使用了一种极其将就的方法，详见2025-tmp-docs/libctest-glibc修复日志
        put_data(
            token,
            siginfo_addr as *mut SigInfo,
            SigInfo::new(signo as u32, 0, (-6 as i32) as u32, task.pid() as u32),
        );
        // a1
        trap_cx.set_a1(siginfo_addr);

        user_sp = sig_sp;
        // 是 sigInfo
        user_sp = user_sp - size_of::<usize>();
        put_data(token, user_sp as *mut usize, usize::MAX);
    }

    // checkout(Magic Num)
    user_sp -= size_of::<usize>();
    put_data(token, user_sp as *mut usize, 0xdeadbeef);
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
                if #[cfg(feature = "loongarch64")] {
                    trampoline = memory_layout::sigreturn_va();
                } else if #[cfg(feature = "riscv64")] {
                    trampoline = sigreturn_trampoline as *const() as usize;
                }
            }
            //warn!("set sigreturn_trampoline={:#x} as ra", trampoline);
            trampoline
        },
    );

    task_inner.sig_mask |= sig_action.act.sa_mask | SigSet::from_sig(signo);
}
/// 恢复栈帧
pub fn restore_frame() -> SyscallRet {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();

    let token = task.process.inner_lock().get_locked_memory_set_read().token();

    let trap_cx = task_inner.trap_cx();
    let mut user_sp = trap_cx.get_sp();

    let checkout = get_data(token, user_sp as *const usize);
    assert!(checkout == 0xdeadbeef, "restore frame checkout error!");
    user_sp += size_of::<usize>();

    // sigInfo标志位
    let sa_siginfo = get_data(token, user_sp as *const usize) == usize::MAX;
    user_sp += size_of::<usize>();

    if !sa_siginfo {
        // signal mask
        task_inner.sig_mask = get_data(token, user_sp as *const SigSet);
        user_sp += size_of::<SigSet>();
        // Trap cx
        let mctx = get_data(token, user_sp as *const MachineContext);
        trap_cx.copy_from_mctx(mctx);
    } else {
        let uctx_addr = user_sp as usize + size_of::<SigInfo>();
        debug!("load: uctx_addr = {:#x}", uctx_addr);
        let uctx: UserContext = get_data(token, uctx_addr as *mut UserContext);
        task_inner.sig_mask = uctx.sigmask;
        let mctx = uctx.mcontext;
        trap_cx.copy_from_mctx(mctx);
    }
    debug!("[restore_frame!] sepc= {:#x}", trap_cx.get_sepc());
    Ok(trap_cx.get_a0())
}

/// 向task的inner的sig_pending按位或一个signal
fn add_signal(task: &TaskControlBlock, signal: SigSet) {
    let mut task_inner = task.inner_lock();
    debug!("add signal: tid {}, signal: {}", task.tid(), signal.bits());
    task_inner.sig_pending |= signal;
}

/// 向进程/线程组发信号
/// 如果未找到这个进程/线程组，会return
pub fn send_signal_to_thread_group(pid: usize, sig: SigSet) -> Result<usize, SysErrNo> {
    let process = Process::get_process_arc_by_pid(pid);
    if let Some(proc) = process {
        let tasks = &proc.meta_lock().tasks;
        for task in tasks.iter() {
            if let Some(task) = task.upgrade() {
                add_signal(&task, sig);
            }
        }
        return Ok(0);
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
