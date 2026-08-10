//! 用户态 signal frame 构造与恢复。
//!
//! 这里负责把当前 trap context、signal mask、`siginfo_t`/`ucontext_t`
//! 写入用户栈，并在 `rt_sigreturn` 时恢复现场。信号选择、默认动作和投递
//! 不在本模块处理。

use core::mem::size_of;

use log::warn;

#[cfg(feature = "fault-diagnostics")]
use super::SIGSEGV;
use super::{
    KSigAction, NormalSignalFrame, SigActionFlags, SigInfo, SigInfoSignalFrame, SigSet, SignalStack,
};
use crate::{
    arch::{
        context::{MachineContext, UserContext},
        memory_layout::{self, USER_STACK_SIZE},
        __PAD_SIZE,
    },
    mm::{copy_from_user_val, copy_to_user_val, probe_user_write, VirtAddr},
    task::{current_task, exit_current_and_run_next},
    utils::{SysErrNo, SyscallRet},
};

extern "C" {
    pub fn sigreturn_trampoline();
}

const SIGNAL_STACK_ALIGN: usize = 16;
const SIGNAL_FRAME_MAGIC: usize = 0xdeadbeef;

const NORMAL_FRAME_MCONTEXT_OFFSET: usize = core::mem::offset_of!(NormalSignalFrame, mcontext);
const SIGINFO_FRAME_SIGINFO_OFFSET: usize = core::mem::offset_of!(SigInfoSignalFrame, siginfo);
const SIGINFO_FRAME_UCONTEXT_OFFSET: usize = core::mem::offset_of!(SigInfoSignalFrame, ucontext);
const SIGINFO_FRAME_MCONTEXT_OFFSET: usize =
    SIGINFO_FRAME_UCONTEXT_OFFSET + core::mem::offset_of!(UserContext, mcontext);

// The frame representation is part of the user ABI.  In particular,
// MachineContext is 16-byte aligned on LoongArch because it contains LSX
// registers, so deriving the siginfo mcontext offset is safer than carrying a
// hard-coded value through diagnostics and restore code.
const _: () = {
    assert!(
        NORMAL_FRAME_MCONTEXT_OFFSET
            == 2 * size_of::<usize>() + size_of::<SignalStack>() + size_of::<SigSet>()
    );
    assert!(
        size_of::<NormalSignalFrame>()
            == 2 * size_of::<usize>()
                + size_of::<SignalStack>()
                + size_of::<SigSet>()
                + size_of::<MachineContext>()
    );
    assert!(SIGINFO_FRAME_SIGINFO_OFFSET == 2 * size_of::<usize>());
    assert!(SIGINFO_FRAME_UCONTEXT_OFFSET == 2 * size_of::<usize>() + size_of::<SigInfo>());
    assert!(
        SIGINFO_FRAME_MCONTEXT_OFFSET
            == 2 * size_of::<usize>()
                + size_of::<SigInfo>()
                + core::mem::offset_of!(UserContext, mcontext)
    );
    assert!(
        size_of::<SigInfoSignalFrame>()
            == 2 * size_of::<usize>() + size_of::<SigInfo>() + size_of::<UserContext>()
    );
};

#[inline]
fn saved_mcontext_pc(mcontext: &MachineContext) -> usize {
    // Both supported MachineContext ABIs store the interrupted PC in their
    // first word: LoongArch GeneralRegs.pc and RISC-V's temporary x[0].
    unsafe { *(mcontext as *const MachineContext as *const usize) }
}

fn verify_signal_frame_values(
    memory_set: &crate::mm::MemorySet,
    signal_sp: usize,
    expected_siginfo_flag: usize,
    expected_pc: usize,
    actual_magic: usize,
    actual_siginfo_flag: usize,
    actual_pc: usize,
) -> bool {
    if actual_magic == SIGNAL_FRAME_MAGIC
        && actual_siginfo_flag == expected_siginfo_flag
        && actual_pc == expected_pc
    {
        return true;
    }

    warn!(
        "setup_frame: signal frame verification failed at sp={:#x}, expected magic={:#x} flag={:#x} pc={:#x}, read magic={:#x} flag={:#x} pc={:#x}, memory_set_token={:#x}",
        signal_sp,
        SIGNAL_FRAME_MAGIC,
        expected_siginfo_flag,
        expected_pc,
        actual_magic,
        actual_siginfo_flag,
        actual_pc,
        memory_set.token(),
    );
    false
}

fn verify_normal_signal_frame_write(
    memory_set: &crate::mm::MemorySet,
    signal_sp: usize,
    expected_pc: usize,
) -> bool {
    let frame = match copy_from_user_val::<NormalSignalFrame>(
        memory_set,
        signal_sp as *const NormalSignalFrame,
    ) {
        Ok(frame) => frame,
        Err(err) => {
            warn!(
                "setup_frame: cannot read back normal signal frame at sp={:#x}, error={:?}, memory_set_token={:#x}",
                signal_sp,
                err,
                memory_set.token(),
            );
            return false;
        }
    };
    verify_signal_frame_values(
        memory_set,
        signal_sp,
        0,
        expected_pc,
        frame.magic,
        frame.siginfo_flag,
        saved_mcontext_pc(&frame.mcontext),
    )
}

fn verify_siginfo_signal_frame_write(
    memory_set: &crate::mm::MemorySet,
    signal_sp: usize,
    expected_pc: usize,
) -> bool {
    let frame = match copy_from_user_val::<SigInfoSignalFrame>(
        memory_set,
        signal_sp as *const SigInfoSignalFrame,
    ) {
        Ok(frame) => frame,
        Err(err) => {
            warn!(
                "setup_frame: cannot read back SA_SIGINFO signal frame at sp={:#x}, error={:?}, memory_set_token={:#x}",
                signal_sp,
                err,
                memory_set.token(),
            );
            return false;
        }
    };
    verify_signal_frame_values(
        memory_set,
        signal_sp,
        usize::MAX,
        expected_pc,
        frame.magic,
        frame.siginfo_flag,
        saved_mcontext_pc(&frame.ucontext.mcontext),
    )
}

/// 在用户态栈空间构建一个 Frame。
///
/// 构建这个帧的目的就是为了执行完信号处理程序后返回到内核态，
/// 并恢复原来内核栈的内容。
pub fn setup_frame(signo: usize, sig_action: KSigAction, siginfo: Option<SigInfo>) {
    // debug!("handler sa_handler={:#x}", sig_action.act.sa_handler);
    let task = current_task().unwrap();
    let proc = &task.process;
    // SA_RESETHAND: 在调用信号处理函数之前将 handler 重置为 SIG_DFL
    // 这样信号处理函数仅在第一次收到信号时被调用
    if sig_action
        .act
        .sa_flags
        .contains(SigActionFlags::SA_RESETHAND)
    {
        proc.with_sigtable(|sigtable| {
            sigtable.set_action(signo, KSigAction::default_action());
        });
    }

    // Do not hold the task lock while faulting/COWing user pages. Signal frame
    // construction only needs a snapshot; commit the updated context below.
    let (mut trap_cx, active_sig_mask, restore_sig_mask, alt_signal_stack) = {
        let mut task_inner = task.inner_lock();
        let active_sig_mask = task_inner.sig_mask;
        // 唤醒 rt_sigsuspend() 的 handler 需要同时保留两套 mask：
        //
        // - `active_sig_mask` 是当前临时 mask。handler 运行时以它为基础，
        //   再叠加 sa_mask；未设置 SA_NODEFER 时还会屏蔽 signo。
        // - `restore_sig_mask` 是调用 rt_sigsuspend() 前的 mask。它保存到
        //   当前 frame，并在 handler 返回时由 rt_sigreturn() 恢复。
        //
        // 仅由第一个 frame 通过 take() 消费恢复值；嵌套 handler 应保存外层
        // handler 的当前 mask，不能重复使用该旧值。
        let restore_sig_mask = task_inner
            .sigsuspend_restore_mask
            .take()
            .unwrap_or(active_sig_mask);
        (
            *task_inner.trap_cx(),
            active_sig_mask,
            restore_sig_mask,
            task_inner.alt_signal_stack,
        )
    };
    let interrupted_sp = trap_cx.get_sp();

    let memory_set = proc.memory_set_arc();
    let interrupted_on_alt_stack = alt_signal_stack.is_on_stack(interrupted_sp);
    let (user_sp, stack_bottom) = if sig_action.act.sa_flags.contains(SigActionFlags::SA_ONSTACK)
        && alt_signal_stack.should_switch_for_signal(interrupted_sp)
    {
        let Some(stack_top) = alt_signal_stack.stack_top() else {
            warn!(
                "setup_frame: alternate stack overflows for signal {}, base={:#x}, size={:#x}",
                signo, alt_signal_stack.sp, alt_signal_stack.size
            );
            drop(memory_set);
            drop(task);
            exit_current_and_run_next((super::SIGSEGV + 128) as i32);
            return;
        };
        (stack_top, alt_signal_stack.sp)
    } else if interrupted_on_alt_stack {
        (interrupted_sp, alt_signal_stack.sp)
    } else {
        // Dynamically find the MapArea containing the interrupted SP.  This
        // also handles thread stacks allocated through mmap.
        let sp_vpn = VirtAddr::from(interrupted_sp).floor();
        let stack_bottom = memory_set
            .get_ref()
            .areas
            .iter()
            .find(|area| area.vpn_range.start() <= sp_vpn && sp_vpn < area.vpn_range.end())
            .map(|area| VirtAddr::from(area.vpn_range.start()).0)
            .unwrap_or_else(|| interrupted_sp.saturating_sub(USER_STACK_SIZE));
        (interrupted_sp, stack_bottom)
    };

    let raw_frame_size = if sig_action.act.sa_flags.contains(SigActionFlags::SA_SIGINFO) {
        size_of::<SigInfoSignalFrame>()
    } else {
        size_of::<NormalSignalFrame>()
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

    // Build a complete contiguous image and write it with one large copy.  In
    // particular, do not split the same frame between the direct small-copy
    // uaccess path and the software PPN/COW path.  The full copy is larger
    // than the direct-copy threshold, so it also uses one consistent software
    // translation path for all frame fields.
    let signal_sp = frame_start;
    let expected_pc = trap_cx.get_sepc();
    if !sig_action.act.sa_flags.contains(SigActionFlags::SA_SIGINFO) {
        // 普通 handler: void (*sa_handler)(int)。用户栈从低到高布局为：
        // [magic][siginfo 标记 = 0][SignalStack][SigSet][MachineContext]。
        // Save the configured stack_t state, rather than the dynamic
        // sigaltstack(2) query view.  Linux exposes SS_ONSTACK to a live query
        // but preserves the raw configuration in a signal frame.
        let frame = NormalSignalFrame {
            magic: SIGNAL_FRAME_MAGIC,
            siginfo_flag: 0,
            stack: alt_signal_stack,
            sigmask: restore_sig_mask,
            mcontext: trap_cx.as_mctx(),
        };
        if copy_to_user_val(&*memory_set, signal_sp as *mut NormalSignalFrame, &frame).is_err() {
            return signal_frame_write_failed(signo, signal_sp, task);
        }
        if !verify_normal_signal_frame_write(&memory_set, signal_sp, expected_pc) {
            return signal_frame_write_failed(signo, signal_sp, task);
        }
    } else {
        // SA_SIGINFO handler: void (*sa_sigaction)(int, siginfo_t *, void *)。
        // 用户栈从低到高布局为：[magic][标记 = usize::MAX][SigInfo][UserContext]。
        // UserContext 与 SigInfo 必须相邻，rt_sigreturn 依赖这一固定相对偏移。
        let uctx = UserContext {
            flags: 0,
            link: 0,
            stack: alt_signal_stack,
            sigmask: restore_sig_mask,
            __pad: [0u8; __PAD_SIZE],
            mcontext: trap_cx.as_mctx(),
        };
        let frame = SigInfoSignalFrame {
            magic: SIGNAL_FRAME_MAGIC,
            siginfo_flag: usize::MAX,
            siginfo: siginfo.unwrap_or_else(|| SigInfo::new(signo as u32, 0, 0, 0)),
            ucontext: uctx,
        };
        if copy_to_user_val(&*memory_set, signal_sp as *mut SigInfoSignalFrame, &frame).is_err() {
            return signal_frame_write_failed(signo, signal_sp, task);
        }
        if !verify_siginfo_signal_frame_write(&memory_set, signal_sp, expected_pc) {
            return signal_frame_write_failed(signo, signal_sp, task);
        }
        // 第三个 handler 参数 a2 指向用户栈中的 UserContext。
        trap_cx.set_a2(signal_sp + SIGINFO_FRAME_UCONTEXT_OFFSET);
        // 第二个 handler 参数 a1 指向用户栈中的 SigInfo。
        trap_cx.set_a1(signal_sp + SIGINFO_FRAME_SIGINFO_OFFSET);
    }
    // a0
    trap_cx.set_a0(signo);
    // sp
    trap_cx.set_sp(signal_sp);
    // 修改Trap
    trap_cx.set_sepc(sig_action.act.sa_handler);
    // ra
    let restorer = if sig_action
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
        trampoline
    };
    #[cfg(feature = "fault-diagnostics")]
    if signo == SIGSEGV {
        warn!(
            "[fault-diagnostics] sigsegv_frame pid={} tid={} handler_sepc={:#x} interrupted_sp={:#x} handler={:#x} sa_flags={:#x} sa_restore={:#x} selected_restorer={:#x} frame_sp={:#x} on_alt_stack={}",
            task.pid(),
            task.tid(),
            trap_cx.get_sepc(),
            interrupted_sp,
            sig_action.act.sa_handler,
            sig_action.act.sa_flags.bits(),
            sig_action.act.sa_restore,
            restorer,
            signal_sp,
            interrupted_on_alt_stack,
        );
    }
    trap_cx.set_ra(restorer);

    // 默认：在处理函数执行期间阻塞当前信号 + sa_mask 中的信号
    // SA_NODEFER: 不自动阻塞当前信号
    let mut new_mask = sig_action.act.sa_mask;
    if !sig_action.act.sa_flags.contains(SigActionFlags::SA_NODEFER) {
        new_mask |= SigSet::from_sig(signo);
    }
    let mut task_inner = task.inner_lock();
    *task_inner.trap_cx() = trap_cx;
    task_inner.sig_mask = active_sig_mask | new_mask;
    // Each frame saves the prior stack state, so rt_sigreturn can restore an
    // SS_AUTODISARM configuration after this handler completes.
    if alt_signal_stack.is_autodisarm() {
        task_inner.alt_signal_stack = SignalStack::disabled();
    }
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

/// 临时诊断：rt_sigreturn 帧 magic 校验失败时，输出帧内保存的现场和 sp 附近内存，
/// 用于定位帧被破坏的根因（handler 覆盖、sp 不对、setup_frame 写错位置等）。
/// 定位完成后删除本函数及其调用点。
fn dump_invalid_sigreturn_frame(
    task: &alloc::sync::Arc<crate::task::TaskControlBlock>,
    signal_sp: usize,
    checkout: usize,
    memory_set: &crate::mm::MemorySet,
) {
    let read_word = |addr: usize| -> Option<usize> {
        copy_from_user_val(memory_set, addr as *const usize).ok()
    };
    // rt_sigreturn 陷入内核时保存的用户寄存器快照：syscall_pc 在 trampoline
    // 页内（0xffff_ffff_fffe_4c4c）说明是 handler 返回路径；否则是用户代码
    // 显式调用 rt_sigreturn。
    let (syscall_pc, user_ra, user_fp) = {
        let inner = task.inner_lock();
        let tc = inner.trap_cx();
        (tc.get_sepc(), tc.get_ra(), tc.get_fp())
    };
    let flag = read_word(signal_sp + size_of::<usize>());
    // 诊断偏移必须与用户 ABI 共用同一组结构计算：LoongArch 的
    // MachineContext 具有 16 字节对齐，不能把 UserContext 的字段字节和
    // mcontext 的实际起点混为一谈。
    let (mctx_off, kind) = match flag {
        Some(usize::MAX) => (SIGINFO_FRAME_MCONTEXT_OFFSET, "siginfo"),
        _ => (NORMAL_FRAME_MCONTEXT_OFFSET, "normal"),
    };
    // MachineContext 内保存用户 sp 的字段偏移：loongarch gp.sp 在 +0x18，
    // riscv64 GeneralRegs.x[2] 在 +0x10。
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "loongarch64")] {
            let mctx_sp_off = 0x18usize;
        } else {
            let mctx_sp_off = 0x10usize;
        }
    }
    let flag_txt = flag
        .map(|v| alloc::format!("{:#x}", v))
        .unwrap_or_else(|| "unreadable".into());
    warn!(
        "[fault-diagnostics] bad rt_sigreturn pid={} tid={} checkout={:#x} sp={:#x} kind={} flag={} syscall_pc={:#x} user_ra={:#x} user_fp={:#x} saved_pc={:#x} saved_sp={:#x}",
        task.pid(),
        task.tid(),
        checkout,
        signal_sp,
        kind,
        flag_txt,
        syscall_pc,
        user_ra,
        user_fp,
        read_word(signal_sp + mctx_off).unwrap_or(0),
        read_word(signal_sp + mctx_off + mctx_sp_off).unwrap_or(0),
    );
    // 扫描 sp 上下是否存在真实帧 magic，定位 signal_sp 与 rt_sigreturn sp 的差。
    let scan_start = signal_sp.saturating_sub(0x1000);
    let scan_end = signal_sp.saturating_add(0x800);
    let mut magic_hits = 0usize;
    let mut scan_off = scan_start;
    while scan_off < scan_end {
        if read_word(scan_off) == Some(SIGNAL_FRAME_MAGIC) {
            magic_hits += 1;
            let delta_txt = if scan_off >= signal_sp {
                alloc::format!("+{:#x}", scan_off - signal_sp)
            } else {
                alloc::format!("-{:#x}", signal_sp - scan_off)
            };
            warn!(
                "[fault-diagnostics] bad rt_sigreturn FOUND magic at {:#x} (delta {})",
                scan_off, delta_txt
            );
            let around = scan_off.saturating_sub(0x40);
            let mut line = alloc::string::String::new();
            for j in (0..0x90usize).step_by(8) {
                match read_word(around + j) {
                    Some(v) => line.push_str(&alloc::format!(" {:016x}", v)),
                    None => line.push_str(" ????????????????"),
                }
            }
            warn!(
                "[fault-diagnostics] bad rt_sigreturn frame[{:#x}..{:#x}):{}",
                around,
                around + 0x90,
                line
            );
        }
        scan_off += 8;
    }
    if magic_hits == 0 {
        warn!(
            "[fault-diagnostics] bad rt_sigreturn no {:#x} in [{:#x}, {:#x})",
            SIGNAL_FRAME_MAGIC, scan_start, scan_end
        );
    }
    // sp-0x400 起连续输出 0x740 字节（覆盖 sp 下方可能存在的真实帧 + 期望帧区域）。
    let start = signal_sp.saturating_sub(0x400);
    let mut line = alloc::string::String::new();
    for i in (0..0x740usize).step_by(8) {
        match read_word(start + i) {
            Some(v) => line.push_str(&alloc::format!(" {:016x}", v)),
            None => line.push_str(" ????????????????"),
        }
        if (i / 8) % 8 == 7 {
            warn!(
                "[fault-diagnostics] bad rt_sigreturn mem[{:#x}..{:#x}):{}",
                start + i - 0x38,
                start + i + 8,
                line
            );
            line.clear();
        }
    }
    if !line.is_empty() {
        warn!(
            "[fault-diagnostics] bad rt_sigreturn mem[{:#x}..{:#x}):{}",
            start,
            start + 0x740,
            line
        );
    }
}

/// 恢复栈帧。
pub fn restore_frame() -> SyscallRet {
    let task = current_task().unwrap();
    let signal_sp = {
        let task_inner = task.inner_lock();
        task_inner.trap_cx().get_sp()
    };
    let memory_set = task.process.memory_set_arc();
    let mut user_sp = signal_sp;

    // 用户态可能不带合法 frame 直接调用 rt_sigreturn，或在 handler 返回前
    // 破坏 sp 处的 frame。magic 校验和后续 frame 解析都是可失败的：校验失败
    // 按 rt_sigreturn(2) 的语义返回 -EINVAL，不能 panic 内核，也不应终止进程。
    let checkout: usize =
        copy_from_user_val(&*memory_set, user_sp as *const usize).map_err(|_| SysErrNo::EINVAL)?;
    if checkout != SIGNAL_FRAME_MAGIC {
        warn!(
            "restore_frame: invalid frame magic {:#x} at sp={:#x}; rt_sigreturn returns EINVAL",
            checkout, signal_sp
        );
        dump_invalid_sigreturn_frame(&task, signal_sp, checkout, &memory_set);
        return Err(SysErrNo::EINVAL);
    }
    user_sp += size_of::<usize>();

    // sigInfo标志位
    let sa_siginfo_flag: usize =
        copy_from_user_val(&*memory_set, user_sp as *const usize).map_err(|_| SysErrNo::EINVAL)?;
    let sa_siginfo = sa_siginfo_flag == usize::MAX;
    user_sp += size_of::<usize>();

    let (restored_sig_mask, restored_stack, restored_mctx) = if !sa_siginfo {
        let saved_stack: SignalStack =
            copy_from_user_val(&*memory_set, user_sp as *const SignalStack)
                .map_err(|_| SysErrNo::EINVAL)?;
        user_sp += size_of::<SignalStack>();
        let sig_mask = copy_from_user_val(&*memory_set, user_sp as *const SigSet)
            .map_err(|_| SysErrNo::EINVAL)?;
        user_sp += size_of::<SigSet>();
        let mctx = copy_from_user_val(&*memory_set, user_sp as *const MachineContext)
            .map_err(|_| SysErrNo::EINVAL)?;
        (sig_mask, saved_stack, mctx)
    } else {
        let uctx_addr = user_sp as usize + size_of::<SigInfo>();
        // debug!("load: uctx_addr = {:#x}", uctx_addr);
        let uctx: UserContext = copy_from_user_val(&*memory_set, uctx_addr as *const UserContext)
            .map_err(|_| SysErrNo::EINVAL)?;
        (uctx.sigmask, uctx.stack, uctx.mcontext)
    };

    drop(memory_set);
    let mut task_inner = task.inner_lock();
    task_inner.sig_mask = restored_sig_mask;
    let (restored_sp, return_value) = {
        let trap_cx = task_inner.trap_cx();
        trap_cx.copy_from_mctx(restored_mctx);
        // The Linux ABI judges whether a replacement is allowed against the
        // restored interrupted stack pointer, not the signal frame's stack
        // pointer.  The latter is necessarily on the alternate stack for a
        // SA_ONSTACK handler.
        (trap_cx.get_sp(), trap_cx.get_a0())
    };
    if let Ok(stack) = task_inner
        .alt_signal_stack
        .replace_from_user(restored_stack, restored_sp)
    {
        task_inner.alt_signal_stack = stack;
    }
    Ok(return_value)
}
