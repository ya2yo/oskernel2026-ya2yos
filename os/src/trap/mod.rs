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
        check_if_any_sig_for_current_task, handle_signal, send_signal_to_thread, SigSet, SIGSEGV,
    },
    syscall::{syscall, Syscall},
    task::{
        check_all_task_timers, check_timer_events, current_task, current_token, current_trap_cx,
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
            // jump to next instruction anyway
            let mut cx = current_trap_cx();
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
                    send_signal_to_thread(tid, SigSet::SIGSEGV);
                    return;
                }
            };
            let mut ok;
            {
                let task = current_task().unwrap();
                let process = task.process.inner_lock();
                let memory_set = process.get_locked_memory_set_read();
                ok = memory_set.lazy_page_fault(fault_va.floor(), cause); // ok表示是否是lazy_page_fault，如果不是返回 false
                if !ok {
                    ok = memory_set.cow_page_fault(fault_va.floor(), cause);
                }
                // drop task inner and task to avoid deadlock and exit exception
            }
            if !ok {
                // Always send SIGSEGV and let the signal mechanism decide:
                // - custom handler → setup_frame, jump to user handler
                // - default action  → terminate (128 + signo)
                let tid = current_task().unwrap().tid();
                warn!(
                    "[kernel] hart {} {:?} in application, bad addr = {:#x}, bad instruction = {:#x}, sending SIGSEGV.",
                    hartid,
                    cause,
                    stval,
                    current_trap_cx().get_sepc(),
                );
                send_signal_to_thread(tid, SigSet::SIGSEGV);
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
                send_signal_to_thread(tid, SigSet::SIGSEGV);
                return;
            };
            let ok;
            {
                let task = current_task().unwrap();
                let process = task.process.inner_lock();
                let memory_set = process.get_locked_memory_set_read();
                ok = memory_set.cow_page_fault(fault_va.floor(), cause);
            }
            if !ok {
                tlb_page_modify_handler();
            }
        }
        Trap::Exception(Exception::PagePrivilegeIllegal) => {
            // 页面存在但权限不足（如用户态访问内核页、写只读页等）
            // 这不是 lazy page fault 或 COW 能修复的，直接发送 SIGSEGV
            let tid = current_task().unwrap().tid();
            warn!(
                "[kernel] hart {} PagePrivilegeIllegal in application, bad addr = {:#x}, bad instruction = {:#x}, sending SIGSEGV.",
                hartid,
                stval,
                current_trap_cx().get_sepc(),
            );
            send_signal_to_thread(tid, SigSet::SIGSEGV);
            return;
        }

        Trap::Interrupt(Interrupt::Timer) => {
            check_timer_events();
            check_all_task_timers();
            // 检查futex操作是否超时
            check_futex_timer();
            set_next_trigger();
            // debug!("Timer Interupt!");
            suspend_current_and_run_next();
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
    current_task().unwrap().check_timer();

    //记录内核空间花费CPU时间，同时准备用户空间花费CPU时间
    current_task()
        .unwrap()
        .inner_lock()
        .time_data
        .update_stime();
}

#[no_mangle]
pub fn trap_return() {
    //检查信号
    while let Some(signo) = check_if_any_sig_for_current_task() {
        // 默认信号可以连续消费；遇到用户自定义 handler 时需要立刻返回用户态，
        // 让用户 handler 先运行，避免在同一个 trap_return 中覆盖信号栈帧。
        let customed = current_task()
            .unwrap()
            .process
            .inner_lock()
            .get_locked_sigtable()
            .action(signo)
            .customed;
        handle_signal(signo);
        if customed {
            break;
        }
    }
    set_user_trap_entry();
    extern "C" {
        #[allow(improper_ctypes)]
        fn __return_to_user(cx: *mut TrapContext);
    }
    // 启动任务的页表
    current_task()
        .unwrap()
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .activate();

    unsafe {
        // 方便调试进入__return_to_user
        let trap_cx = current_trap_cx();
        // let ptr = (trap_cx as *mut TrapContext) as usize;
        // debug!(
        //     "### [return_to_user], trap_cx.sepc={:#x}, sp={:#x}, kstack={:#x}, trap_cx={:#x}",
        //     trap_cx.get_sepc(),
        //     trap_cx.get_sp(),
        //     trap_cx.kernel_stack,
        //     ptr
        // );
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
    let stval_vpn = VirtAddr::from(stval).floor();
    panic!(
        "stval = {:#x}(vpn {}), 
        a trap {:?} from kernel!",
        stval, stval_vpn.0, cause
    );
}

#[cfg(target_arch = "loongarch64")]
#[no_mangle]
pub fn trap_from_kernel() {
    use log::error;

    let cause = get_trap_cause();
    if cause == Trap::Interrupt(Interrupt::Timer) {
        // 中断返回？
        return;
    }

    backtrace();
    let stval = get_trap_virt_addr();
    let stval_vpn = VirtAddr::from(stval).floor();
    error!(
        "stval = {:#x}(vpn {}), 
        a trap {:?} from kernel!",
        stval, stval_vpn.0, cause
    );
    loop {}
}
