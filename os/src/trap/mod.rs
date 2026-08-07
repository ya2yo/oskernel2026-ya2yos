//! Trap handling functionality
//!
//! For rCore, we have a single trap entry point, namely `__alltraps`. At
//! initialization in [`init()`], we set the `stvec` CSR to point to it.
//!
//! All traps go through `__alltraps`, which is defined in `trap.S`. The
//! assembly language code does just enough work restore the kernel space
//! context, ensuring that Rust code safely runs, and transfers control to
//! [`trap_handler()`].
//!
//! It then calls different functionality based on what exactly the exception
//! was. For example, timer interrupts trigger task preemption, and syscalls go
//! to [`syscall()`].

pub mod trap_types;
use core::panic::PanicInfo;

use crate::{
    arch::{cpu::hart_id, page_table::PageTable, trap_interface::tlb_page_modify_handler},
    mm::{VirtAddr, VirtPageNum},
    signal::{
        check_if_any_sig_for_current_task, deliver_itimer_signal, handle_signal,
        send_signal_to_thread, SigSet,
    },
    syscall::{syscall, Syscall},
    task::{
        check_timer_events, current_task, current_token, current_trap_cx,
        exit_current_and_run_next, suspend_current_and_run_next,
    },
    timer::{check_futex_timer, set_next_trigger},
    utils::backtrace,
};
use log::{debug, trace, warn};

use crate::arch::trap_interface::{
    enable_timer_interrupt, get_trap_cause, get_trap_virt_addr, set_kernel_trap_entry,
    set_user_trap_entry, trap_init,
};
use trap_types::{Exception, Interrupt, Trap};

use crate::arch::context::*;

extern "C" {
    fn __trap_from_user();
}
/// initialize CSR `stvec` as the entry of `__alltraps`
pub fn init() {
    trap_init();
}

/// Emit one snapshot only after the normal fault handler has failed and the
/// task is about to receive a synchronous fault signal. This is deliberately
/// feature-gated: normal lazy allocation faults can be extremely frequent.
#[cfg(feature = "fault-diagnostics")]
fn log_user_fault_signal(
    hartid: usize,
    cause: Trap,
    stval: usize,
    fault_va: Option<VirtAddr>,
    signal: SigSet,
) {
    let task = current_task().unwrap();
    let (tid, pid, sepc, sp, return_sstatus) = {
        let task_inner = task.inner_lock();
        let return_sstatus = Some(task_inner.trap_cx().get_status_bits());
        (
            task.tid(),
            task.pid(),
            task_inner.trap_cx().get_sepc(),
            task_inner.trap_cx().get_sp(),
            return_sstatus,
        )
    };
    let active_page_table_token = crate::arch::page_table::get_token_from_regs();
    let diagnostic = fault_va.map(|fault_va| {
        task.process
            .memory_set_arc()
            .fault_diagnostic(fault_va.floor())
    });
    let memory_set_token = diagnostic.map(|diagnostic| diagnostic.page_table_token);
    let pte_flags_bits = diagnostic.and_then(|diagnostic| diagnostic.pte_flags_bits);
    let pte_leaf_level = diagnostic.and_then(|diagnostic| diagnostic.pte_leaf_level);
    let pte_raw_bits = diagnostic.and_then(|diagnostic| diagnostic.pte_raw_bits);
    let pte_leaf_ppn = diagnostic.and_then(|diagnostic| diagnostic.pte_leaf_ppn);
    warn!(
        "[fault-diagnostics] user_fault_signal hart={} pid={} tid={} cause={:?} stval={:#x} sepc={:#x} sp={:#x} signal={:?} return_sstatus={:x?} active_page_table_token={:#x} memory_set_token={:x?} pte_flags={:x?} pte_leaf_level={:?} pte_raw={:x?} pte_leaf_ppn={:x?} diagnostic={:?}",
        hartid,
        pid,
        tid,
        cause,
        stval,
        sepc,
        sp,
        signal,
        return_sstatus,
        active_page_table_token,
        memory_set_token,
        pte_flags_bits,
        pte_leaf_level,
        pte_raw_bits,
        pte_leaf_ppn,
        diagnostic,
    );
}

#[no_mangle]
/// handle an interrupt, exception, or system call from user space
pub fn trap_handler() {
    //记录用户空间花费CPU时间，同时准备内核空间花费CPU时间
    current_task()
        .unwrap()
        .inner_lock()
        .time_data
        .update_utime();

    let hartid = hart_id();

    set_kernel_trap_entry();
    let cause = get_trap_cause();
    let stval = get_trap_virt_addr();
    // debug!(
    //     "### [trap_handler]: scause={:?}, stval={:#x}, sepc={:#x}",
    //     cause,
    //     stval,
    //     current_trap_cx().get_sepc()
    // );
    match cause {
        Trap::Exception(Exception::Syscall) => {
            #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
            current_task()
                .unwrap()
                .inner_lock()
                .clear_present_page_fault_retry();
            // jump to next instruction anyway
            let mut cx = current_trap_cx();
            cx.origin_a0 = cx.get_a0();
            cx.sepc_step(4);
            let syscall_id = Syscall::from(cx.get_syscall_id());
            // get system call return value
            let result = syscall(cx.get_syscall_id(), cx.get_syscall_args());
            // cx is changed during sys_exec, so we have to call it again
            cx = current_trap_cx();
            cx.set_a0(match result {
                Ok(res) => res,
                Err(errno) => -(errno as isize) as usize,
            });
            // 打印结果
            match result {
                Ok(ret) => debug!("[syscall ret --- OK] {:?} ret = {}", syscall_id, ret),
                Err(errno) => debug!(
                    "[syscall ret --- Err] {:?} ret = {}",
                    syscall_id,
                    errno.str()
                ),
            }
        }
        Trap::Exception(Exception::StorePageFault)
        | Trap::Exception(Exception::LoadPageFault)
        | Trap::Exception(Exception::FetchInstructionPageFault) => {
            //debug!("{:?},bad addr = {:#x}", scause.cause(), stval);
            // page fault
            let fault_va = match VirtAddr::try_from(stval) {
                Some(va) => va,
                None => {
                    let tid = current_task().unwrap().tid();
                    warn!(
                        "[kernel] hart {} {:?} in application, non-canonical bad addr = {:#x}, bad instruction = {:#x}, sending SIGSEGV.",
                        hartid,
                        cause,
                        stval,
                        current_trap_cx().get_sepc(),
                    );
                    #[cfg(feature = "fault-diagnostics")]
                    log_user_fault_signal(hartid, cause, stval, None, SigSet::SIGSEGV);
                    send_signal_to_thread(tid, SigSet::SIGSEGV);
                    return;
                }
            };
            let signal;
            {
                let task = current_task().unwrap();
                let process = &task.process;
                let memory_set = process.memory_set_arc();
                let beyond_eof_before = memory_set.mmap_file_page_beyond_eof(fault_va.floor());
                let handled =
                    !beyond_eof_before && memory_set.handle_page_fault(fault_va.floor(), cause);
                #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
                // If the PTE already permits this user load/fetch but the CPU
                // still faulted, another thread may have just installed the
                // page while this fault was in progress. Allow one retry
                // after flushing the stale TLB entry; the task-local VPN
                // guard prevents an endless retry loop for a real fault.
                let retry_present_user_fault = !handled
                    && matches!(
                        cause,
                        Trap::Exception(Exception::LoadPageFault)
                            | Trap::Exception(Exception::FetchInstructionPageFault)
                    )
                    && ((cause == Trap::Exception(Exception::LoadPageFault)
                        && memory_set.is_user_readable(fault_va.floor()))
                        || (cause == Trap::Exception(Exception::FetchInstructionPageFault)
                            && memory_set.is_user_executable(fault_va.floor())))
                    && task.inner_lock().retry_present_page_fault(fault_va.floor());
                #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
                let retry_present_user_fault = false;
                signal = if beyond_eof_before {
                    Some(SigSet::SIGBUS)
                } else if handled {
                    #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
                    task.inner_lock().clear_present_page_fault_retry();
                    None
                } else if retry_present_user_fault {
                    #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
                    {
                        crate::arch::tlb::tlb_invalidate();
                        if cause == Trap::Exception(Exception::FetchInstructionPageFault) {
                            crate::arch::tlb::instruction_fence();
                        }
                    }
                    None
                } else if memory_set.mmap_file_page_beyond_eof(fault_va.floor()) {
                    // The file may have been truncated after the initial
                    // check but before the page-cache load. Recheck only on
                    // failure so that this race still reports SIGBUS without
                    // adding a second cache load to the hot path.
                    Some(SigSet::SIGBUS)
                } else {
                    Some(SigSet::SIGSEGV)
                };
                // drop task inner and task to avoid deadlock and exit exception
            }
            if let Some(signal) = signal {
                // The VMA/EOF distinction determines SIGBUS versus SIGSEGV;
                // signal delivery then invokes a custom handler or terminates.
                let tid = current_task().unwrap().tid();
                #[cfg(feature = "fault-diagnostics")]
                log_user_fault_signal(hartid, cause, stval, Some(fault_va), signal);
                send_signal_to_thread(tid, signal);
                return;
            }
        }
        Trap::Exception(Exception::IllegalInstruction) => {
            backtrace();
            warn!(
                "[kernel] [hart {}] IllegalInstruction at {:#x} in application, kernel killed it.",
                hartid,
                current_trap_cx().get_sepc(),
            );
            // illegal instruction exit code
            exit_current_and_run_next(-3);
            panic!("You should not return from exit_current_and_run_next");
        }
        Trap::Exception(Exception::PageModifyFault) => {
            let Some(fault_va) = VirtAddr::try_from(stval) else {
                let tid = current_task().unwrap().tid();
                warn!(
                    "[kernel] hart {} PageModifyFault in application, non-canonical bad addr = {:#x}, bad instruction = {:#x}, sending SIGSEGV.",
                    hartid,
                    stval,
                    current_trap_cx().get_sepc(),
                );
                #[cfg(feature = "fault-diagnostics")]
                log_user_fault_signal(hartid, cause, stval, None, SigSet::SIGSEGV);
                send_signal_to_thread(tid, SigSet::SIGSEGV);
                return;
            };
            let ok;
            {
                let task = current_task().unwrap();
                let process = &task.process;
                let memory_set = process.memory_set_arc();
                ok = memory_set.handle_page_fault(fault_va.floor(), cause);
            }
            if !ok {
                tlb_page_modify_handler();
            }
        }
        Trap::Exception(Exception::PagePrivilegeIllegal) => {
            let fault_va = match VirtAddr::try_from(stval) {
                Some(fault_va) => fault_va,
                None => {
                    let tid = current_task().unwrap().tid();
                    warn!(
                        "[kernel] hart {} PagePrivilegeIllegal in application, non-canonical bad addr = {:#x}, bad instruction = {:#x}, sending SIGSEGV.",
                        hartid,
                        stval,
                        current_trap_cx().get_sepc(),
                    );
                    #[cfg(feature = "fault-diagnostics")]
                    log_user_fault_signal(hartid, cause, stval, None, SigSet::SIGSEGV);
                    send_signal_to_thread(tid, SigSet::SIGSEGV);
                    return;
                }
            };
            let signal;
            {
                let task = current_task().unwrap();
                let process = &task.process;
                let memory_set = process.memory_set_arc();
                signal = if memory_set.mmap_file_page_beyond_eof(fault_va.floor()) {
                    Some(SigSet::SIGBUS)
                } else if memory_set.handle_page_fault(fault_va.floor(), cause) {
                    None
                } else if memory_set.mmap_file_page_beyond_eof(fault_va.floor()) {
                    // See the load-fault path above: distinguish a truncate
                    // race from an ordinary protection or mapping failure.
                    Some(SigSet::SIGBUS)
                } else {
                    Some(SigSet::SIGSEGV)
                };
            }
            if let Some(signal) = signal {
                // Page permission faults can also be the first access to a
                // beyond-EOF file page on LoongArch, which requires SIGBUS.
                let tid = current_task().unwrap().tid();
                warn!(
                    "[kernel] hart {} PagePrivilegeIllegal in application, bad addr = {:#x}, bad instruction = {:#x}, sending {:?}.",
                    hartid,
                    stval,
                    current_trap_cx().get_sepc(),
                    signal,
                );
                #[cfg(feature = "fault-diagnostics")]
                log_user_fault_signal(hartid, cause, stval, Some(fault_va), signal);
                send_signal_to_thread(tid, signal);
                return;
            }
        }

        Trap::Interrupt(Interrupt::Timer) => {
            check_timer_events();
            deliver_itimer_signal(&current_task().unwrap());
            // 检查futex操作是否超时
            check_futex_timer();
            set_next_trigger();
            // debug!("Timer Interupt!");
            crate::task::preempt_current_and_run_next();
        }
        Trap::Interrupt(Interrupt::Ipi) => {
            #[cfg(target_arch = "riscv64")]
            crate::arch::cpu::clear_ipi();
            #[cfg(target_arch = "loongarch64")]
            crate::arch::cpu::clear_ipi();
            crate::mm::remote_tlb::poll();
            crate::task::migrate_current_if_needed();
        }
        // Trap::Exception(Exception::Breakpoint) => {
        //     warn!("[kernel] Breakpoint from application");
        //     // jump to next instruction anyway
        //     let cx = current_trap_cx();
        //     cx.sepc_step(2);
        // }
        _ => {
            panic!("Unsupported trap {:?}, stval = {:#x}!", cause, stval);
        }
    }
    //检查定时器
    deliver_itimer_signal(&current_task().unwrap());

    //记录内核空间花费CPU时间，同时准备用户空间花费CPU时间
    current_task()
        .unwrap()
        .inner_lock()
        .time_data
        .update_stime();
}

/// Publish rseq CPU state and abort a live rseq critical section before user
/// execution resumes.  A stale or malformed user rseq area is fatal in Linux;
/// disable it here before queueing SIGSEGV so the signal path cannot retry an
/// inaccessible pointer indefinitely.
fn prepare_rseq_user_return(force: bool) -> bool {
    let task = current_task().unwrap();
    let tid = task.tid();
    match task.rseq_prepare_user_return(force) {
        Ok(()) => true,
        Err(errno) => {
            warn!(
                "rseq user-return fixup failed for tid {}: {:?}; sending SIGSEGV",
                tid, errno
            );
            task.disable_rseq();
            drop(task);
            let _ = send_signal_to_thread(tid, SigSet::SIGSEGV);
            false
        }
    }
}

#[no_mangle]
pub fn trap_return() {
    loop {
        if let Some(signo) = check_if_any_sig_for_current_task() {
            // The signal frame must save the rseq abort PC, not the interrupted
            // critical-section PC.  Do this before handle_signal/setup_frame.
            let rseq_ok = prepare_rseq_user_return(true);
            // 默认信号可以连续消费；遇到用户自定义 handler 时需要立刻返回用户态，
            // 让用户 handler 先运行，避免在同一个 trap_return 中覆盖信号栈帧。
            let has_handler = current_task()
                .unwrap()
                .process
                .with_sigtable(|sigtable| sigtable.action(signo).is_handler());
            handle_signal(signo);
            if has_handler && rseq_ok {
                break;
            }
            continue;
        }

        // Timer preemption and migration both resume through trap_return.
        // This common point publishes the actual hart and aborts a live rseq
        // critical section before any instruction can run in user mode.
        if prepare_rseq_user_return(false) {
            // rt_sigsuspend() 有 handler 时由 rt_sigreturn 恢复旧 mask。若
            // trap return 仅消费默认/忽略信号，就没有 signal frame 可负责恢复。
            let task = current_task().unwrap();
            let mut task_inner = task.inner_lock();
            if let Some(old_sig_mask) = task_inner.sigsuspend_restore_mask.take() {
                task_inner.sig_mask = old_sig_mask;
            }
            break;
        }
    }
    extern "C" {
        #[allow(improper_ctypes)]
        fn __return_to_user(cx: *mut TrapContext);
    }
    // 启动任务的页表
    crate::mm::remote_tlb::poll();
    current_task()
        .unwrap()
        .process
        .memory_set_arc()
        .activate_for_user();

    unsafe {
        // 方便调试进入__return_to_user
        let trap_cx = current_trap_cx();
        #[cfg(target_arch = "riscv64")]
        {
            // Every TaskControlBlock resumes a user process. A corrupted or
            // stale SPP=Supervisor would make sret execute the user text in
            // S-mode, where a valid U=1 executable PTE still raises an
            // instruction page fault. Keep the architectural return mode
            // explicit at the final common return point.
            current_trap_cx()
                .sstatus
                .set_spp(riscv::register::sstatus::SPP::User);
            trap_cx.kernel_hartid = hart_id();

            // __trap_from_user treats sscratch as a TrapContext pointer. Keep
            // S-mode interrupts disabled until sret makes the user trap entry
            // active, otherwise a kernel interrupt could be decoded as a user
            // trap and overwrite the saved hart id/context.
            riscv::register::sstatus::clear_sie();
        }
        // let ptr = (trap_cx as *mut TrapContext) as usize;
        // debug!(
        //     "### [return_to_user], trap_cx.sepc={:#x}, sp={:#x}, kstack={:#x}, trap_cx={:#x}",
        //     trap_cx.get_sepc(),
        //     trap_cx.get_sp(),
        //     trap_cx.kernel_stack,
        //     ptr
        // );
        // This must be immediately followed by the non-returning user restore.
        set_user_trap_entry();
        __return_to_user(trap_cx as *mut TrapContext);
    }
    panic!("You should not return from __return_to_user!");
}

#[no_mangle]
pub extern "C" fn trap_entry() {
    // debug!("trap_entry!!!");
    trap_handler();
    trap_return();
}

#[cfg(target_arch = "riscv64")]
#[no_mangle]
pub extern "C" fn trap_from_kernel_frame(_frame: *mut usize) {
    use riscv::register::{sepc, stval};

    let cause = get_trap_cause();
    let stval_val = stval::read();
    match crate::mm::uaccess::handle_kernel_fault(cause, stval_val) {
        crate::mm::uaccess::KernelFaultAction::Retry => return,
        crate::mm::uaccess::KernelFaultAction::Fixup(fixup_pc) => {
            unsafe {
                core::arch::asm!("csrw sepc, {}", in(reg) fixup_pc, options(nostack));
            }
            return;
        }
        crate::mm::uaccess::KernelFaultAction::Unhandled => {}
    }

    let sepc_val = sepc::read();
    let ra: usize;
    let sp: usize;
    unsafe {
        core::arch::asm!("mv {}, ra", out(reg) ra);
        core::arch::asm!("mv {}, sp", out(reg) sp);
    }
    println!("\n---- KERNEL PANIC IN S-MODE ----");
    println!("Cause: {:?}", cause);
    println!("stval: {:#x}", stval_val);
    println!("sepc : {:#x}", sepc_val);
    println!("ra   : {:#x}", ra);
    println!("sp   : {:#x}", sp);
    println!("--------------------------------\n");
    backtrace();
    let stval = get_trap_virt_addr();
    let stval_vpn = VirtAddr::try_from(stval).map(|va| va.floor().0);
    panic!(
        "stval = {:#x}(vpn {:?}),
        a trap {:?} from kernel!",
        stval, stval_vpn, cause
    );
}

#[cfg(target_arch = "riscv64")]
#[no_mangle]
pub fn trap_from_kernel() -> ! {
    use riscv::register::{sepc, stval};

    let cause = get_trap_cause();
    let stval_val = stval::read();
    let sepc_val = sepc::read(); // 发生异常的那条指令的地址

    // 获取崩溃时的 ra 和 sp
    let ra: usize;
    let sp: usize;
    unsafe {
        core::arch::asm!("mv {}, ra", out(reg) ra);
        core::arch::asm!("mv {}, sp", out(reg) sp);
    }
    println!("\n---- KERNEL PANIC IN S-MODE ----");
    println!("Cause: {:?}", cause);
    println!("stval: {:#x}", stval_val);
    println!("sepc : {:#x}", sepc_val); // 如果 sepc 是 0，那就是跳到了空地址
    println!("ra   : {:#x}", ra); // 这个 ra 往往指向崩溃函数的调用者
    println!("sp   : {:#x}", sp); // 查看 sp 是否在合法的内核栈范围内
    println!("--------------------------------\n");
    if cause == Trap::Interrupt(Interrupt::Timer) {
        // 中断返回？
    }

    backtrace();
    let stval = get_trap_virt_addr();
    let stval_vpn = VirtAddr::try_from(stval).map(|va| va.floor().0);
    panic!(
        "stval = {:#x}(vpn {:?}),
        a trap {:?} from kernel!",
        stval, stval_vpn, cause
    );
}

#[cfg(target_arch = "loongarch64")]
#[no_mangle]
pub extern "C" fn trap_from_kernel(frame: *mut usize) {
    let cause = get_trap_cause();
    let stval = get_trap_virt_addr();
    match crate::mm::uaccess::handle_kernel_fault(cause, stval) {
        crate::mm::uaccess::KernelFaultAction::Retry => return,
        crate::mm::uaccess::KernelFaultAction::Fixup(fixup_pc) => {
            unsafe { frame.write(fixup_pc) };
            return;
        }
        crate::mm::uaccess::KernelFaultAction::Unhandled => {}
    }
    match cause {
        Trap::Interrupt(Interrupt::Timer) => return,
        Trap::Interrupt(Interrupt::Ipi) => {
            crate::arch::cpu::clear_ipi();
            crate::mm::remote_tlb::poll();
            return;
        }
        _ => {}
    }

    use log::error;
    backtrace();
    let stval_vpn = VirtAddr::from(stval).floor();
    error!(
        "stval = {:#x}(vpn {}), 
        a trap {:?} from kernel!",
        stval, stval_vpn.0, cause
    );
    loop {}
}
