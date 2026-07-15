//! 用户态 signal frame 构造与恢复。
//!
//! 这里负责把当前 trap context、signal mask、`siginfo_t`/`ucontext_t`
//! 写入用户栈，并在 `rt_sigreturn` 时恢复现场。信号选择、默认动作和投递
//! 不在本模块处理。

use core::mem::size_of;

use log::{error, warn};

use super::{KSigAction, SigActionFlags, SigInfo, SigSet, SignalStack};
use crate::{
    arch::{
        context::{MachineContext, UserContext},
        memory_layout::{self, USER_STACK_SIZE},
    },
    mm::{copy_from_user_val, copy_to_user, copy_to_user_val, VirtAddr},
    task::{current_task, exit_current_and_run_next},
    utils::{SysErrNo, SyscallRet},
};

extern "C" {
    pub fn sigreturn_trampoline();
}

/// 在用户态栈空间构建一个 Frame。
///
/// 构建这个帧的目的就是为了执行完信号处理程序后返回到内核态，
/// 并恢复原来内核栈的内容。
pub fn setup_frame(signo: usize, sig_action: KSigAction, siginfo: Option<SigInfo>) {
    // debug!("handler sa_handler={:#x}", sig_action.act.sa_handler);
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    // SA_RESETHAND: 在调用信号处理函数之前将 handler 重置为 SIG_DFL
    // 这样信号处理函数仅在第一次收到信号时被调用
    if sig_action
        .act
        .sa_flags
        .contains(SigActionFlags::SA_RESETHAND)
    {
        proc_inner.with_sigtable(|sigtable| {
            sigtable.set_action(signo, KSigAction::default_action());
        });
    }

    let mut task_inner = task.inner_lock();
    let trap_cx = task_inner.trap_cx();
    let mut user_sp = trap_cx.get_sp();

    // 动态查找包含当前 sp 的 MapArea，以此确定栈的真实边界。
    // 这对 mmap 分配的线程栈也能正确工作。
    let memory_set = proc_inner.memory_set_arc();
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
        drop(task);
        exit_current_and_run_next((signo + 128) as i32);
    } else {
        // if this syscall wants to restart
        let restart_errno = -(SysErrNo::ERESTART as isize) as usize;
        if trap_cx.get_a0() == restart_errno {
            // ERESTART 是 wait/select 等阻塞 syscall 与信号分发之间的内核内部状态。
            // signal frame 在 trap_return 阶段构造，不能再依赖当前 CSR trap cause
            // 判断是否来自 syscall；只要 trap context 中仍保留 -ERESTART，就必须
            // 在保存用户 mcontext 前把它转换成重启或 EINTR 语义，避免 errno 85
            // 暴露给用户态。
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
                trap_cx.set_a0(-(SysErrNo::EINTR as isize) as usize);
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
                &siginfo.unwrap_or_else(|| SigInfo::new(signo as u32, 0, 0, 0)),
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

/// 恢复栈帧。
pub fn restore_frame() -> SyscallRet {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();

    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

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
