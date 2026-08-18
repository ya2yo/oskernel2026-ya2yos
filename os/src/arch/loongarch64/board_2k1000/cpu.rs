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
const MAX_UBOOT_GO_ARGS: usize = 16;
const MAX_UBOOT_GO_ARG_LEN: usize = 32;
const FDT_MAGIC: u32 = 0xd00d_feed;
const MAX_EFI_SYSTEM_TABLE_OFFSET: usize = 0x1_0000_0000;
// The U-Boot `fdt_addr` environment value on this board. It lies in the
// first RAM bank and does not overlap U-Boot's relocation area or the kernel.
const UBOOT_FDT_STAGING_ADDR: usize = KERNEL_ADDR_OFFSET + 0x0a00_0000;
// This board's vendor U-Boot relocates its control FDT to this stable address.
// Prefer it so raw `go` boot does not depend on the vendor's argument ABI.
const BOARD_CONTROL_FDT_ADDR: usize = 0x9000_0000_0ecc_f480;
const UART_BASE: usize = KERNEL_ADDR_OFFSET + 0x1fe2_0000;

// Kept in .data so rust_main's bootstrap BSS clear cannot erase the FDT that
// a secondary hart needs after being released through the IOCSR mailbox.
static BOOT_FDT_ADDR: AtomicUsize = AtomicUsize::new(usize::MAX);

#[inline]
pub(crate) fn early_uart_marker(byte: u8) {
    // The vendor U-Boot configures this UART before `go`. Do not wait for its
    // line-status register here: a marker must never create a second hang.
    unsafe { (UART_BASE as *mut u8).write_volatile(byte) };
}

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

#[inline]
fn is_direct_mapped_address(addr: usize) -> bool {
    addr & 0xffff_0000_0000_0000 == KERNEL_ADDR_OFFSET
}

fn direct_mapped_pointer(addr: usize) -> Option<usize> {
    if is_direct_mapped_address(addr) {
        Some(addr)
    } else if addr < KERNEL_ADDR_OFFSET {
        addr.checked_add(KERNEL_ADDR_OFFSET)
    } else {
        None
    }
}

fn parse_hex_address(arg: usize) -> Option<usize> {
    let ptr = direct_mapped_pointer(arg)? as *const u8;
    let mut index = 0;
    if unsafe { ptr.read() } == b'0' && matches!(unsafe { ptr.add(1).read() }, b'x' | b'X') {
        index = 2;
    }

    let mut value = 0usize;
    let mut has_digit = false;
    while index < MAX_UBOOT_GO_ARG_LEN {
        let byte = unsafe { ptr.add(index).read() };
        if byte == 0 {
            return has_digit.then_some(value);
        }
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        } as usize;
        value = value.checked_mul(16)?.checked_add(digit)?;
        has_digit = true;
        index += 1;
    }
    None
}

fn valid_fdt_address(addr: usize) -> Option<usize> {
    let Some(addr) = direct_mapped_pointer(addr) else {
        return None;
    };
    if addr & 3 == 0
        && unsafe { core::ptr::read_unaligned(addr as *const u32).to_be() } == FDT_MAGIC
    {
        Some(addr)
    } else {
        None
    }
}

fn fdt_from_uboot_go(argc: usize, argv: usize) -> Option<usize> {
    if !(1..=MAX_UBOOT_GO_ARGS).contains(&argc) {
        return None;
    }

    let argv = direct_mapped_pointer(argv)? as *const usize;
    // U-Boot variants disagree on whether the command name is included in
    // the array passed to the application. Scan every argument instead of
    // requiring argv[0] to be the entry address.
    for index in 0..argc {
        if let Some(fdt) = parse_hex_address(unsafe { argv.add(index).read() }) {
            if let Some(fdt) = valid_fdt_address(fdt) {
                return Some(fdt);
            }
        }
    }
    None
}

fn looks_like_uboot_go_call(argc: usize, argv: usize) -> bool {
    (1..=MAX_UBOOT_GO_ARGS).contains(&argc) && direct_mapped_pointer(argv).is_some()
}

#[no_mangle]
pub fn init_csr_regs(boot_arg0: usize, boot_arg1: usize, system_table_offset: usize) {
    early_uart_marker(b'C');
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
    early_uart_marker(b'D');

    // Prefer the known control-FDT address on this vendor board. This avoids
    // depending on whether the vendor's `go` implementation preserves the
    // standard argc/argv handoff.
    if let Some(fdt) = valid_fdt_address(BOARD_CONTROL_FDT_ADDR) {
        BOOT_FDT_ADDR.store(fdt, Ordering::Release);
    } else if let Some(fdt) = fdt_from_uboot_go(boot_arg0, boot_arg1) {
        BOOT_FDT_ADDR.store(fdt, Ordering::Release);
    } else if let Some(fdt) = valid_fdt_address(system_table_offset) {
        // Some firmware wrappers pass the complete FDT pointer in a2 rather
        // than exposing it through the U-Boot argv array.
        BOOT_FDT_ADDR.store(fdt, Ordering::Release);
    } else if let Some(fdt) = valid_fdt_address(UBOOT_FDT_STAGING_ADDR) {
        // A board-side `fdt move ${fdtcontroladdr} ${fdt_addr} 0x10000`
        // gives raw `go` launches a stable FDT handoff even when a vendor
        // U-Boot overrides do_go_exec and drops argc/argv.
        BOOT_FDT_ADDR.store(fdt, Ordering::Release);
    } else if !looks_like_uboot_go_call(boot_arg0, boot_arg1)
        && system_table_offset != 0
        && system_table_offset & 7 == 0
        && system_table_offset < MAX_EFI_SYSTEM_TABLE_OFFSET
    {
        if let Some(fdt) = crate::arch::hardware::loongarch_fdt_from_efi(system_table_offset) {
            BOOT_FDT_ADDR.store(fdt, Ordering::Release);
        }
    }
    early_uart_marker(b'E');

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
    early_uart_marker(b'F');

    let fdt = loop {
        let fdt = BOOT_FDT_ADDR.load(Ordering::Acquire);
        if fdt != usize::MAX {
            break fdt;
        }
        core::hint::spin_loop();
    };
    early_uart_marker(b'G');
    rust_main(hart_id(), fdt);
}
