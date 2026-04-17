use core::arch::global_asm;
use log::{error, warn};
use riscv::register::{
    mtvec::TrapMode,
    scause, sepc, sie,
    sstatus::{self, FS},
    stval, stvec,
};

use crate::trap::trap_types::{Exception, Interrupt, Trap};

/// 中断原因
pub fn get_trap_cause() -> Trap {
    scause_to_trap(scause::read().cause())
}

fn scause_to_trap(value: scause::Trap) -> Trap {
    match value {
        scause::Trap::Interrupt(interrupt) => match interrupt {
            scause::Interrupt::SupervisorTimer => Trap::Interrupt(Interrupt::Timer),
            _ => Trap::Unknown,
        },
        scause::Trap::Exception(exception) => match exception {
            scause::Exception::LoadPageFault => Trap::Exception(Exception::LoadPageFault),
            scause::Exception::StorePageFault => Trap::Exception(Exception::StorePageFault),
            scause::Exception::InstructionPageFault => {
                Trap::Exception(Exception::FetchInstructionPageFault)
            }
            scause::Exception::UserEnvCall => Trap::Exception(Exception::Syscall),
            _ => {
                error!(
                    "Fail to convert RISCV scause({:?}) to TatlinOS Trap type!",
                    value
                );
                Trap::Unknown
            }
        },
    }
}

/// 用户试图访问而不得的虚拟地址
pub fn get_trap_virt_addr() -> usize {
    stval::read()
}

#[inline]
pub fn set_kernel_trap_entry() {
    extern "C" {
        pub fn trap_from_kernel() -> !;
    }
    unsafe {
        stvec::write(trap_from_kernel as usize, TrapMode::Direct);
    }
}
#[inline]
pub fn set_user_trap_entry() {
    extern "C" {
        fn __trap_from_user();
    }
    unsafe {
        stvec::write(__trap_from_user as usize, TrapMode::Direct);
    }
}

/// enable timer interrupt in sie CSR
pub fn enable_timer_interrupt() {
    unsafe {
        sie::set_stimer();
    }
}

pub fn trap_init() {
    set_kernel_trap_entry();
    //开启rustsbi的浮点指令
    unsafe {
        sstatus::set_fs(FS::Clean);
    }
}

/// RISCV下这个函数不应该被调用
pub fn tlb_page_modify_handler() {
    // do nothing
    // 实际上，你不应该运行到这里的
}
