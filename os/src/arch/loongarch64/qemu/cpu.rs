use super::{
    memory_layout::PAGE_SIZE_BITS,
    // trap::{set_kernel_trap_entry, trap_handler},
    trap_interface::set_kernel_trap_entry,
};
use crate::trap::trap_handler;
use crate::{
    arch::{config::HART_NUM, memory_layout::PAGE_SIZE},
    rust_main,
};
use core::arch::asm;
use loongArch64::ipi::{csr_mail_send, send_ipi_single};
use loongArch64::register::prcfg1::{self, Prcfg1};
use loongArch64::register::{asid, tlbidx};
use loongArch64::register::{
    cpuid, crmd, dmw0, dmw1, dmw2, dmw3, ecfg, eentry, euen, prmd,
    pwch::{self, set_dir3_base},
    pwcl, stlbps, tcfg, ticlr, tlbrehi, tlbrentry, CpuMode, MemoryAccessType,
};

const BOOT_IPI_VECTOR: u32 = 1 << 0;

// LA库似乎有点问题，没把这个暴露出来……
fn set_merrentry(val: usize) {
    assert!(val & 0xFFF == 0, "set_merrentry: not aligned");
    unsafe {
        asm!("csrwr {}, 0x93", in(reg)val);
    }
}

pub fn shutdown(_failure: bool) -> ! {
    unsafe {
        ((0x9000_0000_100e_001c) as *mut u8).write_volatile(0x34);
    }
    loop {}
}

/// 获取当前运行的 CPU 核
pub fn hart_id() -> usize {
    // $tp is restored from the user trap context, so it cannot be used as
    // persistent per-hart state after returning from userspace.
    cpuid::read().core_id()
}

/// Start QEMU LoongArch secondary harts through the mailbox/IPI boot ROM.
pub fn boot_secondary_harts(boot_hart: usize) {
    extern "C" {
        fn _start();
    }

    // QEMU loads CPU0 at the high-half ELF entry and its slave boot ROM must
    // jump to that same address. Direct-address translation supplies the PA.
    let entry = _start as *const () as usize as u64;
    for hart in 0..HART_NUM {
        if hart == boot_hart {
            continue;
        }
        csr_mail_send(entry, hart, 0);
        send_ipi_single(hart, BOOT_IPI_VECTOR);
    }
}

/// LoongArch's current kernel-mode trap entry is not resumable, so it cannot
/// enable interrupts around `idle` yet.  Keep the existing polling behavior
/// until that entry gains a complete save/restore path.
pub fn idle() {
    core::hint::spin_loop();
}

/// 初始化csr寄存器
#[no_mangle]
pub fn init_csr_regs() {
    println!("init_csr_regs");
    println!("init_csr_regs: save num = {}", prcfg1::read().save_num());
    // 设置直接地址翻译模式下的映射窗口
    // 只使用dmw0映射0x9000...开头的地址
    dmw0::set_plv0(true);
    dmw0::set_plv1(false);
    dmw0::set_plv2(false);
    dmw0::set_plv3(false);
    dmw0::set_mat(MemoryAccessType::StronglyOrderedUnCached); // 映射在这个地址的都是mmio，不经过缓存
    dmw0::set_vseg(9);

    dmw1::set_plv0(false);
    dmw1::set_plv1(false);
    dmw1::set_plv2(false);
    dmw1::set_plv3(false);
    dmw2::set_plv0(false);
    dmw2::set_plv1(false);
    dmw2::set_plv2(false);
    dmw2::set_plv3(false);
    dmw3::set_plv0(false);
    dmw3::set_plv1(false);
    dmw3::set_plv2(false);
    dmw3::set_plv3(false);

    // 设置中断
    ticlr::clear_timer_interrupt(); // 清除定时器中断，
    tcfg::set_en(false); // 关闭定时器
    crmd::set_ie(false); // 关闭全局中断

    // 设置当前模式寄存器
    // 设置特权级，启用分页
    crmd::set_plv(CpuMode::Ring0); // 设置特权级为0
    crmd::set_da(false); // 启用分页（和set_pg一起发挥作用）
    crmd::set_pg(true);
    crmd::set_datf(MemoryAccessType::CoherentCached); // 直接翻译的取指访存类型
    crmd::set_datm(MemoryAccessType::CoherentCached); // 直接翻译的load/store访存类型
    crmd::set_we(true); // 启用硬件监视点

    // 设置异常入口地址
    set_merrentry(trap_handler as *const () as usize); // 机器异常入口
    extern "C" {
        fn __tlb_rfill();
    }
    tlbrentry::set_tlbrentry(__tlb_rfill as *const () as usize); // tlb重填异常
    set_kernel_trap_entry(); // 其他异常。设置的是eentry，返回用户态时，会set_user_trap_entry();

    // 设置页表参数
    tlbidx::set_ps(PAGE_SIZE_BITS);
    stlbps::set_ps(PAGE_SIZE_BITS);
    println!("stlb={},{}", stlbps::read().raw(), stlbps::read().ps());
    tlbrehi::set_ps(PAGE_SIZE_BITS);
    pwcl::set_pte_width(8); // 该函数有问题，这里8代表8字节是可以的

    // 页表结构配置
    pwcl::set_ptbase(12);
    pwcl::set_ptwidth(9);
    pwcl::set_dir1_base(12 + 9);
    pwcl::set_dir1_width(9);
    pwcl::set_dir2_base(12 + 9 + 9);
    pwcl::set_dir2_width(9);
    pwch::set_dir3_base(0);
    pwch::set_dir3_width(0);
    pwch::set_dir4_base(0);
    pwch::set_dir4_width(0);

    // 设置PRMD寄存器，使其特权级为3，全局中断使能为开
    // 这样回到用户态时，这些修改就会起效
    prmd::set_pie(true);
    prmd::set_pplv(CpuMode::Ring3);

    // The glibc dynamic loader in the LoongArch test image uses LSX `vld`/
    // `vst` instructions before Bash starts.  LSX requires both the scalar
    // floating-point and SIMD enable bits on every hart.
    euen::set_fpe(true);
    euen::set_sxe(true);

    asid::set_asid_width(0);

    rust_main(hart_id());
}
