//! Kernel virtual memory space initialization.
//!
//! Contains `new_kernel()` for creating the kernel's [`MemorySetInner`] on
//! different architectures, plus the `remap_test()` sanity check.

use super::super::map_area::MapType;
use super::super::memory_set::MemorySetInner;
use super::{MapArea, MapAreaType, MapPermission};
use crate::arch::memory_layout::{MMIO, MMIO_MAP_OFFSET, PAGE_SIZE};
use crate::mm::memory_set::KERNEL_SPACE;

extern "C" {
    fn stext();
    fn etext();
    fn srodata();
    fn erodata();
    fn sdata();
    fn edata();
    fn sbss_with_stack();
    fn ebss();
    fn ekernel();
    fn sigreturn_trampoline();
}

impl MemorySetInner {
    /// Without kernel stacks.
    #[cfg(target_arch = "riscv64")]
    pub fn new_kernel() -> Self {
        let mut memory_set = Self::new_bare();
        println!("kernel token: {:#x}", memory_set.page_table.token());
        println!(
            ".text [{:#x}, {:#x})",
            stext as *const () as usize, etext as *const () as usize
        );
        println!(
            ".rodata [{:#x}, {:#x})",
            srodata as *const () as usize, erodata as *const () as usize
        );
        println!(
            ".data [{:#x}, {:#x})",
            sdata as *const () as usize, edata as *const () as usize
        );
        println!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as *const () as usize, ebss as *const () as usize
        );
        println!(
            "sigreturn_trampoline start: [{:#x}, {:#x}",
            sigreturn_trampoline as *const () as usize,
            sigreturn_trampoline as *const () as usize + PAGE_SIZE
        );

        println!("mapping .text section");
        let s_sig_trap = sigreturn_trampoline as *const () as usize;
        let e_sig_trap = sigreturn_trampoline as *const () as usize + PAGE_SIZE;
        memory_set
            .push(
                MapArea::new(
                    (stext as *const () as usize).into(),
                    (s_sig_trap).into(),
                    MapType::Direct,
                    MapPermission::R | MapPermission::X,
                    MapAreaType::Elf,
                ),
                None,
            )
            .expect("kernel OOM");
        memory_set
            .push(
                MapArea::new(
                    (e_sig_trap).into(),
                    (etext as *const () as usize).into(),
                    MapType::Direct,
                    MapPermission::R | MapPermission::X,
                    MapAreaType::Elf,
                ),
                None,
            )
            .expect("kernel OOM");
        memory_set
            .push(
                MapArea::new(
                    (s_sig_trap).into(),
                    (e_sig_trap).into(),
                    MapType::Direct,
                    MapPermission::R | MapPermission::X | MapPermission::U,
                    MapAreaType::Elf,
                ),
                None,
            )
            .expect("kernel OOM");

        println!("mapping .rodata section");
        memory_set
            .push(
                MapArea::new(
                    (srodata as *const () as usize).into(),
                    (erodata as *const () as usize).into(),
                    MapType::Direct,
                    MapPermission::R,
                    MapAreaType::Elf,
                ),
                None,
            )
            .expect("kernel OOM");

        println!("mapping .data section");
        memory_set
            .push(
                MapArea::new(
                    (sdata as *const () as usize).into(),
                    (edata as *const () as usize).into(),
                    MapType::Direct,
                    MapPermission::R | MapPermission::W,
                    MapAreaType::Elf,
                ),
                None,
            )
            .expect("kernel OOM");

        println!("mapping .bss section");
        memory_set
            .push(
                MapArea::new(
                    (sbss_with_stack as *const () as usize).into(),
                    (ebss as *const () as usize).into(),
                    MapType::Direct,
                    MapPermission::R | MapPermission::W,
                    MapAreaType::Elf,
                ),
                None,
            )
            .expect("kernel OOM");

        println!("mapping physical memory");
        memory_set
            .push(
                MapArea::new(
                    (ekernel as *const () as usize).into(),
                    crate::arch::memory_layout::memory_end().into(),
                    MapType::Direct,
                    MapPermission::R | MapPermission::W,
                    MapAreaType::Physical,
                ),
                None,
            )
            .expect("kernel OOM");

        println!("mapping memory-mapped registers");
        for pair in MMIO {
            let start_va = (*pair).0 + MMIO_MAP_OFFSET;
            let end_va = start_va + (*pair).1;
            memory_set
                .push(
                    MapArea::new(
                        start_va.into(),
                        end_va.into(),
                        MapType::Direct,
                        MapPermission::R | MapPermission::W,
                        MapAreaType::MMIO,
                    ),
                    None,
                )
                .expect("kernel OOM");
        }
        println!("create new kernel successfully!");
        memory_set
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn new_kernel() -> Self {
        let memory_set = Self::new_bare();
        println!("kernel token: {:#x}", memory_set.page_table.token());
        println!(
            ".text [{:#x}, {:#x})",
            stext as *const () as usize, etext as *const () as usize
        );
        println!(
            ".rodata [{:#x}, {:#x})",
            srodata as *const () as usize, erodata as *const () as usize
        );
        println!(
            ".data [{:#x}, {:#x})",
            sdata as *const () as usize, edata as *const () as usize
        );
        println!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as *const () as usize, ebss as *const () as usize
        );
        println!(
            "sigreturn_trampoline start: [{:#x}, {:#x}",
            sigreturn_trampoline as *const () as usize,
            sigreturn_trampoline as *const () as usize + PAGE_SIZE
        );
        println!("create new kernel successfully!");
        memory_set
    }
}

#[allow(unused)]
#[cfg(target_arch = "riscv64")]
pub fn remap_test() {
    println!("remap test start!");
    let mut kernel_space = KERNEL_SPACE.lock();
    kernel_space.page_table.handle_remap_test();
    println!("remap_test passed!");
}

#[allow(unused)]
#[cfg(target_arch = "loongarch64")]
pub fn remap_test() {
    println!("loongarch64 does not need remap_test");
}
