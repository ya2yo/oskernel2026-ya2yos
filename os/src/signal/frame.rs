//! 用户态 signal frame 构造与恢复。
//!
//! 这里负责把当前 trap context、signal mask、`siginfo_t`/`ucontext_t`
//! 写入用户栈，并在 `rt_sigreturn` 时恢复现场。信号选择、默认动作和投递
//! 不在本模块处理。

use core::mem::size_of;

use log::warn;

use super::{KSigAction, SigActionFlags, SigInfo, SigSet, SignalStack};
use crate::{
    arch::{
        context::{MachineContext, UserContext},
        memory_layout::{self, USER_STACK_SIZE},
    },
    mm::{copy_from_user_val, copy_to_user, copy_to_user_val, probe_user_write, VirtAddr},
    task::{current_task, exit_current_and_run_next},
    utils::{SysErrNo, SyscallRet},
};

extern "C" {
    pub fn sigreturn_trampoline();
}

const SIGNAL_STACK_ALIGN: usize = 16;

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

    // Do not hold the task lock while faulting/COWing user pages.  Signal frame
    // construction only needs a snapshot; commit the updated context below.
    let (mut trap_cx, old_sig_mask) = {
        let task_inner = task.inner_lock();
        (*task_inner.trap_cx(), task_inner.sig_mask)
    };
    let user_sp = trap_cx.get_sp();

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

    let raw_frame_size = if sig_action.act.sa_flags.contains(SigActionFlags::SA_SIGINFO) {
        size_of::<UserContext>() + size_of::<SigInfo>() + 2 * size_of::<usize>()
    } else {
        size_of::<MachineContext>() + size_of::<SigSet>() + 2 * size_of::<usize>()
    };
    let Some(raw_frame_start) = user_sp.checked_sub(raw_frame_size) else {
        warn!(
            "setup_frame: signal {} frame underflows user stack, sp={:#x}, stack_bottom={:#x}",
            signo, user_sp, stack_bottom
        );
        drop(memory_set);
        drop(task);
        exit_current_and_run_next((super::SIGSEGV + 128) as i32);
        return;
    };
    // Both supported user ABIs require a 16-byte aligned stack on handler entry.
    // The padding is above the saved context, so rt_sigreturn's frame layout is
    // unchanged.
    // `raw_frame_start` 是不含对齐填充时的 frame 起点；其低位表示还需向下
    // 留出的字节数，才能让最终交给 handler 的 sp 满足 16 字节对齐。
    let frame_padding = raw_frame_start & (SIGNAL_STACK_ALIGN - 1);
    // `frame_size`/`frame_start` 覆盖实际 frame 与其顶部对齐填充，用于完整预检。
    let frame_size = raw_frame_size + frame_padding;
    let frame_start = raw_frame_start - frame_padding;
    // `frame_top` 是实际保存 mcontext/ucontext 的顶部，不包含上方的填充字节。
    let frame_top = user_sp - frame_padding;
    if frame_start < stack_bottom || probe_user_write(&memory_set, frame_start, frame_size).is_err()
    {
        warn!(
            "setup_frame: cannot write signal {} frame, sp={:#x}, frame={:#x}..{:#x}, stack_bottom={:#x}",
            signo,
            user_sp,
            frame_start,
            user_sp,
            stack_bottom
        );
        drop(memory_set);
        drop(task);
        exit_current_and_run_next((super::SIGSEGV + 128) as i32);
        return;
    }

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

    let signal_sp;
    if !sig_action.act.sa_flags.contains(SigActionFlags::SA_SIGINFO) {
        // 普通 handler: void (*sa_handler)(int)。用户栈从高到低布局为：
        // [MachineContext][SigSet][siginfo 标记 = 0][magic]。
        // `signal_sp` 最终指向最低地址的 magic，rt_sigreturn 从此处反向恢复。
        let mctx_addr = frame_top - size_of::<MachineContext>();
        // 保存进入 handler 前的用户寄存器，以便 rt_sigreturn 恢复被打断的执行点。
        let mctx = trap_cx.as_mctx();
        if copy_to_user(&memory_set, mctx_addr, unsafe {
            core::slice::from_raw_parts(
                &mctx as *const MachineContext as *const _,
                core::mem::size_of::<MachineContext>(),
            )
        })
        .is_err()
        {
            return signal_frame_write_failed(signo, mctx_addr, task);
        }

        // 保存进入 handler 前的 signal mask；handler 结束后必须恢复该 mask。
        let sigset_addr = mctx_addr - size_of::<SigSet>();
        if copy_to_user(&memory_set, sigset_addr, unsafe {
            core::slice::from_raw_parts(
                &old_sig_mask as *const SigSet as *const _,
                core::mem::size_of::<SigSet>(),
            )
        })
        .is_err()
        {
            return signal_frame_write_failed(signo, sigset_addr, task);
        }

        // 标记普通 frame。restore_frame() 读到 0 后按 SigSet + MachineContext 解析。
        let siginfo_flag_addr = sigset_addr - size_of::<usize>();
        if copy_to_user(
            &memory_set,
            siginfo_flag_addr,
            &[0u8; core::mem::size_of::<usize>()],
        )
        .is_err()
        {
            return signal_frame_write_failed(signo, siginfo_flag_addr, task);
        }
        signal_sp = siginfo_flag_addr - size_of::<usize>();
    } else {
        // SA_SIGINFO handler: void (*sa_sigaction)(int, siginfo_t *, void *)。
        // 用户栈从高到低布局为：[UserContext][SigInfo][标记 = usize::MAX][magic]。
        // UserContext 与 SigInfo 必须相邻，rt_sigreturn 依赖这一固定相对偏移。
        let uctx_addr = frame_top - size_of::<UserContext>();
        let siginfo_addr = uctx_addr - size_of::<SigInfo>();
        signal_sp = siginfo_addr - 2 * size_of::<usize>();
        let sig_size = siginfo_addr - stack_bottom;
        // debug!("sig_size={:#x}", sig_size);
        // debug!("save: uctx_addr = {:#x}", uctx_addr);
        let uctx = UserContext {
            flags: 0,
            link: 0,
            stack: SignalStack::new(siginfo_addr, sig_size),
            sigmask: old_sig_mask,
            __pad: [0u8; 128],
            mcontext: trap_cx.as_mctx(),
        };
        if copy_to_user(&memory_set, uctx_addr, unsafe {
            core::slice::from_raw_parts(
                &uctx as *const UserContext as *const _,
                core::mem::size_of::<UserContext>(),
            )
        })
        .is_err()
        {
            return signal_frame_write_failed(signo, uctx_addr, task);
        }
        // 第三个 handler 参数 a2 指向用户栈中的 UserContext。
        trap_cx.set_a2(uctx_addr);

        // 第二个 handler 参数所指的 siginfo_t：优先保留投递时记录的发送者信息，
        // 没有附加信息的内核信号则构造零初始化的默认记录。
        if copy_to_user_val(
            &*memory_set,
            siginfo_addr as *mut SigInfo,
            &siginfo.unwrap_or_else(|| SigInfo::new(signo as u32, 0, 0, 0)),
        )
        .is_err()
        {
            return signal_frame_write_failed(signo, siginfo_addr, task);
        }
        // 第二个 handler 参数 a1 指向用户栈中的 SigInfo。
        trap_cx.set_a1(siginfo_addr);

        // 标记 SA_SIGINFO frame。restore_frame() 读到 usize::MAX 后从 SigInfo 后读取 UserContext。
        let siginfo_flag_addr = siginfo_addr - size_of::<usize>();
        if copy_to_user_val(&*memory_set, siginfo_flag_addr as *mut usize, &usize::MAX).is_err() {
            return signal_frame_write_failed(signo, siginfo_flag_addr, task);
        }
    }

    // checkout(Magic Num)
    if copy_to_user_val(&*memory_set, signal_sp as *mut usize, &0xdeadbeefusize).is_err() {
        return signal_frame_write_failed(signo, signal_sp, task);
    }
    // a0
    trap_cx.set_a0(signo);
    // sp
    trap_cx.set_sp(signal_sp);
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
    let mut task_inner = task.inner_lock();
    *task_inner.trap_cx() = trap_cx;
    task_inner.sig_mask = old_sig_mask | new_mask;
}

fn signal_frame_write_failed(
    signo: usize,
    addr: usize,
    task: alloc::sync::Arc<crate::task::TaskControlBlock>,
) {
    warn!(
        "setup_frame: cannot write signal {} frame at {:#x}; terminating task with SIGSEGV",
        signo, addr
    );
    drop(task);
    exit_current_and_run_next((super::SIGSEGV + 128) as i32);
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
