use super::{
    config::HART_NUM,
    memory_layout::{KERNEL_ADDR_OFFSET, PAGE_SIZE_BITS, POWER_OFF_ADDR, POWER_OFF_VALUE},
    trap_interface::set_kernel_trap_entry,
};
use crate::trap::trap_handler;
use crate::{arch::memory_layout::PAGE_SIZE, rust_main};
use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use loongArch64::register::prcfg1::{self, Prcfg1};
use loongArch64::register::{asid, tlbidx};
use loongArch64::register::{
    cpuid, crmd, dmw0, dmw1, dmw2, dmw3,
    ecfg::{self, LineBasedInterrupt},
    eentry, euen, prmd,
    pwch::{self, set_dir3_base},
    pwcl, stlbps, tcfg, ticlr, tlbrehi, tlbrentry, CpuMode, MemoryAccessType,
};
use loongArch64::{
    consts::{LOONGARCH_IOCSR_IPI_CLEAR, LOONGARCH_IOCSR_IPI_EN, LOONGARCH_IOCSR_IPI_STATUS},
    iocsr::{iocsr_read_w, iocsr_write_w},
    ipi::{csr_mail_send, send_ipi_single},
};

const BOOT_IPI_VECTOR: u32 = 1 << 0;
const SCHEDULER_IPI_VECTOR: u32 = 1 << 1;
static BOOT_SYSTEM_TABLE_OFFSET: AtomicUsize = AtomicUsize::new(usize::MAX);

fn set_merrentry(val: usize) {
    assert!(val & 0xfff == 0, "set_merrentry: not aligned");
    unsafe { asm!("csrwr {}, 0x93", in(reg) val) };
}

pub fn shutdown(_failure: bool) -> ! {
    unsafe {
        ((POWER_OFF_ADDR + KERNEL_ADDR_OFFSET) as *mut u32).write_volatile(POWER_OFF_VALUE);
    }
    loop {
        core::hint::spin_loop();
    }
}

pub fn hart_id() -> usize {
    cpuid::read().core_id()
}

pub fn boot_secondary_harts(boot_hart: usize) {
    extern "C" {
        fn _start();
    }

    let entry = _start as *const () as usize as u64;
    for hart in 0..HART_NUM {
        if hart != boot_hart {
            csr_mail_send(entry, hart, 0);
            send_ipi_single(hart, BOOT_IPI_VECTOR);
        }
    }
}

pub fn idle() {
    crmd::set_ie(false);
    ecfg::set_lie(LineBasedInterrupt::TIMER | LineBasedInterrupt::IPI);
    crate::timer::set_next_trigger();
    unsafe {
        core::arch::asm!("idle 0", options(nomem, nostack, preserves_flags));
    }
    ecfg::set_lie(LineBasedInterrupt::TIMER);
    clear_ipi();
    crate::mm::remote_tlb::poll();
    crate::timer::set_next_trigger();
}

pub fn wake_hart(hartid: usize) -> bool {
    if hartid >= HART_NUM {
        return false;
    }
    send_ipi_single(hartid, SCHEDULER_IPI_VECTOR);
    true
}

#[inline]
pub fn clear_ipi() {
    let pending = iocsr_read_w(LOONGARCH_IOCSR_IPI_STATUS);
    if pending != 0 {
        iocsr_write_w(LOONGARCH_IOCSR_IPI_CLEAR, pending);
    }
}

#[no_mangle]
pub fn init_csr_regs(system_table_offset: usize) {
    if system_table_offset != 0 {
        BOOT_SYSTEM_TABLE_OFFSET.store(system_table_offset, Ordering::Release);
    }

    dmw0::set_plv0(true);
    dmw0::set_plv1(false);
    dmw0::set_plv2(false);
    dmw0::set_plv3(false);
    dmw0::set_mat(MemoryAccessType::StronglyOrderedUnCached);
    dmw0::set_vseg(9);
    for dmw in [dmw1::set_plv0, dmw2::set_plv0, dmw3::set_plv0] {
        dmw(false);
    }
    dmw1::set_plv1(false);
    dmw1::set_plv2(false);
    dmw1::set_plv3(false);
    dmw2::set_plv1(false);
    dmw2::set_plv2(false);
    dmw2::set_plv3(false);
    dmw3::set_plv1(false);
    dmw3::set_plv2(false);
    dmw3::set_plv3(false);

    ticlr::clear_timer_interrupt();
    iocsr_write_w(LOONGARCH_IOCSR_IPI_EN, u32::MAX);
    tcfg::set_en(false);
    crmd::set_ie(false);
    crmd::set_plv(CpuMode::Ring0);
    crmd::set_da(false);
    crmd::set_pg(true);
    crmd::set_datf(MemoryAccessType::CoherentCached);
    crmd::set_datm(MemoryAccessType::CoherentCached);
    crmd::set_we(true);

    set_merrentry(trap_handler as *const () as usize);
    extern "C" {
        fn __tlb_rfill();
    }
    tlbrentry::set_tlbrentry(__tlb_rfill as *const () as usize);
    set_kernel_trap_entry();
    tlbidx::set_ps(PAGE_SIZE_BITS);
    stlbps::set_ps(PAGE_SIZE_BITS);
    tlbrehi::set_ps(PAGE_SIZE_BITS);
    pwcl::set_pte_width(8);
    pwcl::set_ptbase(12);
    pwcl::set_ptwidth(9);
    pwcl::set_dir1_base(21);
    pwcl::set_dir1_width(9);
    pwcl::set_dir2_base(30);
    pwcl::set_dir2_width(9);
    pwch::set_dir3_base(0);
    pwch::set_dir3_width(0);
    pwch::set_dir4_base(0);
    pwch::set_dir4_width(0);
    prmd::set_pie(true);
    prmd::set_pplv(CpuMode::Ring3);
    euen::set_fpe(true);
    euen::set_sxe(true);
    asid::set_asid_width(0);

    let system_table_offset = loop {
        let offset = BOOT_SYSTEM_TABLE_OFFSET.load(Ordering::Acquire);
        if offset != usize::MAX {
            break offset;
        }
        core::hint::spin_loop();
    };
    let fdt = crate::arch::hardware::loongarch_fdt_from_efi(system_table_offset)
        .expect("2K1000 bootloader did not provide an EFI FDT table");
    rust_main(hart_id(), fdt);
}
