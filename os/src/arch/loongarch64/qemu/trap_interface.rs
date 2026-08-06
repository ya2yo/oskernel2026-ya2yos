use core::arch::asm;

use log::{debug, error};
use loongArch64::{
    register::{
        badv, crmd,
        ecfg::{self, LineBasedInterrupt},
        eentry, estat, ticlr, tlbelo0, tlbelo1, tlbidx, tlbrbadv,
    },
    time::get_timer_freq,
};

use crate::{
    arch::{page_table::PageTable, tlb::tlb_invalidate},
    mm::{VirtAddr, VirtPageNum},
    task::current_token,
    trap::{
        trap_from_kernel,
        trap_types::{Exception, Interrupt, Trap},
    },
};
pub fn trap_init() {
    // 需要吗？
}
/// 中断原因，相对于riscv中的estat
pub fn get_trap_cause() -> Trap {
    let estat: estat::Estat = estat::read(); // 相当于riscv::estat

    // debug!(
    //     "====ESTATE:{:#x},{:#x},{:#x}====",
    //     estat.ecode(),
    //     estat.esubcode(),
    //     estat.raw()
    // );
    let cause: estat::Trap = estat.cause();
    estat_to_trap(cause)
}

fn estat_to_trap(value: estat::Trap) -> Trap {
    match value {
        estat::Trap::Interrupt(interrupt) => match interrupt {
            estat::Interrupt::Timer => Trap::Interrupt(Interrupt::Timer),
            estat::Interrupt::IPI => Trap::Interrupt(Interrupt::Ipi),
            _ => Trap::Unknown,
        },
        estat::Trap::Exception(exception) => match exception {
            estat::Exception::LoadPageFault => Trap::Exception(Exception::LoadPageFault),
            estat::Exception::StorePageFault => Trap::Exception(Exception::StorePageFault),
            estat::Exception::FetchInstructionAddressError => {
                Trap::Exception(Exception::FetchInstructionPageFault)
            }
            estat::Exception::Syscall => Trap::Exception(Exception::Syscall),
            estat::Exception::FetchPageFault => {
                Trap::Exception(Exception::FetchInstructionPageFault)
            }
            estat::Exception::InstructionNotExist
            | estat::Exception::InstructionPrivilegeIllegal => {
                Trap::Exception(Exception::IllegalInstruction)
            }
            estat::Exception::PageModifyFault => Trap::Exception(Exception::PageModifyFault),
            estat::Exception::PagePrivilegeIllegal => {
                Trap::Exception(Exception::PagePrivilegeIllegal)
            }
            // 以下异常归类为 SIGSEGV 场景，统一映射到 PagePrivilegeIllegal
            // (这些异常意味着页面存在但访问权限不足，应当发 SIGSEGV 给用户进程)
            estat::Exception::PageNonReadableFault
            | estat::Exception::PageNonExecutableFault
            | estat::Exception::MemoryAccessAddressError
            | estat::Exception::AddressNotAligned
            | estat::Exception::BoundsCheckFault => {
                Trap::Exception(Exception::PagePrivilegeIllegal)
            }
            estat::Exception::Breakpoint => {
                // 调试断点：目前未实现 ptrace，跳过该指令继续执行
                debug!("LoongArch Breakpoint exception from user space, ignoring.");
                Trap::Exception(Exception::PagePrivilegeIllegal)
            }
            estat::Exception::FloatingPointUnavailable => {
                // 浮点不可用：发送 SIGFPE 或 SIGILL
                // 目前映射到 IllegalInstruction 让进程终止
                debug!("LoongArch FloatingPointUnavailable from user space.");
                Trap::Exception(Exception::IllegalInstruction)
            }
            estat::Exception::TLBRFill => {
                // TLBRFill 在 estat::read().cause() 中已被优先处理，
                // 理论上不会到达此处，但保留兜底映射
                error!("Unexpected TLBRFill in estat_to_trap!");
                Trap::Exception(Exception::LoadPageFault)
            }
            #[allow(unreachable_patterns)]
            _ => {
                error!("Fail to convert LoongArch estat({:?}) to Trap type!", value);
                Trap::Unknown
            }
        },
        estat::Trap::MachineError(_) => {
            error!("Fail to convert LoongArch MachineError to Trap type!",);
            Trap::Unknown
        }
        estat::Trap::Unknown => {
            error!(
                "Fail to convert LoongArch Unknown to Trap type! {:#x}",
                estat::read().ecode()
            );
            // panic!();
            Trap::Unknown
        }
    }
}

/// 中断发生时的用户程序计数器
pub fn get_trap_pc() -> usize {
    unimplemented!()
}

/// 用户试图访问而不得的虚拟地址
pub fn get_trap_virt_addr() -> usize {
    badv::read().vaddr()
}

#[inline]
pub fn set_kernel_trap_entry() {
    extern "C" {
        fn __kern_trap();
    }
    eentry::set_eentry(__kern_trap as *const () as usize);
}
#[inline]
pub fn set_user_trap_entry() {
    extern "C" {
        fn __alltraps();
    }
    // 设置普通异常和中断入口
    eentry::set_eentry(__alltraps as *const () as usize);
}

pub fn enable_timer_interrupt() {
    ticlr::clear_timer_interrupt();
    // 开启全局中断
    ecfg::set_lie(LineBasedInterrupt::TIMER | LineBasedInterrupt::IPI);
    // crmd::set_ie(true);
}

/// 从proj93-la-tsinghuaOS(https://github.com/Godones/rCoreloongArch)抄来的
/// 用于处理龙芯特有的PageModifyFault
pub fn tlb_page_modify_handler() {
    // INFO!("PageModifyFault handler");
    //找到对应的页表项，修改D位为1
    let badv = badv::read().vaddr(); //出错虚拟地址
    let vpn: VirtAddr = badv.into(); //虚拟地址
    let vpn: VirtPageNum = vpn.floor(); //虚拟地址的虚拟页号
    let token = current_token(); //根页表的地址
    let mut page_table = PageTable::from_token(token);
    page_table.set_dirty_bit(vpn).unwrap();
    // unsafe {
    //     asm!("tlbsrch", "tlbrd",); //根据TLBEHI的虚双页号查询TLB对应项
    // }
    // let tlbidx = tlbidx::read(); //获取TLB项索引
    // assert_eq!(tlbidx.ne(), false); // 为什么？？？

    // let mut tlbelo0 = TLBELO::read(0); //获取TLB项0
    // let mut tlbelo1 = TLBELO::read(1); //获取TLB项1
    // tlbelo0.set_dirty(true).write();
    // tlbelo1.set_dirty(true).write();
    // tlbelo0::set_dirty(true);
    // tlbelo1::set_dirty(true);

    // unsafe {
    //     asm!("tlbwr"); //重新将tlbelo写入tlb
    // }
    tlb_invalidate(); // 牺牲性能保稳定
}
