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
    task::{current_task, current_token},
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
            estat::Exception::InstructionNotExist => {
                debug!("INE Fault, we should stop here for qemu debug");
                let proc = current_task().unwrap().get_process();
                let proc = proc.inner_lock();
                let mem_set = proc.get_locked_memory_set();

                mem_set.activate();
                debug!("translated: {:?}", mem_set.translate_va(0x10000.into()));
                debug!("We are using USER's pagetable now");
                loop {}
                panic!();
            }
            estat::Exception::PageModifyFault => Trap::Exception(Exception::PageModifyFault),
            _ => {
                error!(
                    "Fail to convert LoongArch estat({:?}) to TatlinOS Trap type!",
                    value
                );
                Trap::Unknown
            }
        },
        estat::Trap::MachineError(_) => {
            error!("Fail to convert LoongArch MachineError to TatlinOS Trap type!",);
            Trap::Unknown
        }
        estat::Trap::Unknown => {
            error!(
                "Fail to convert LoongArch Unknown to TatlinOS Trap type! {:#x}",
                estat::read().ecode()
            );
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
    eentry::set_eentry(__kern_trap as usize);
}
#[inline]
pub fn set_user_trap_entry() {
    extern "C" {
        fn __alltraps();
    }
    // 设置普通异常和中断入口
    eentry::set_eentry(__alltraps as usize);
}

pub fn enable_timer_interrupt() {
    ticlr::clear_timer_interrupt();
    // 开启全局中断
    ecfg::set_lie(LineBasedInterrupt::TIMER);
    // crmd::set_ie(true);
}

/// 从proj93-la-tsinghuaOS(https://github.com/Godones/rCoreloongArch)抄来的
/// 用于处理龙芯特有的PageModifyFault
pub fn tlb_page_modify_handler() {
    // INFO!("PageModifyFault handler");
    //找到对应的页表项，修改D位为1
    let badv = tlbrbadv::read().vaddr(); //出错虚拟地址
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
