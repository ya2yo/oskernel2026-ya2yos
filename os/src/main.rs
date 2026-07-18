//! The main module and entrypoint
//!
//! Various facilities of the kernels are implemented as submodules. The most
//! important ones are:
//!
//! - [`trap`]: Handles all cases of switching from userspace to the kernel
//! - [`task`]: Task management
//! - [`syscall`]: System call handling and implementation
//! - [`mm`]: Address map using SV39
//! - [`sync`]: Wrap a static data structure inside it so that we are able to access it without any `unsafe`.
//! - [`fs`]: Separate user from file system with some structures
//! - [`net`]: Network from StarryOS
//! - [`signal`]: Handle signals' transmission
//!
//! The operating system also starts in this module. Kernel code starts
//! executing from `entry.asm`, after which [`rust_main()`] is called to
//! initialize various pieces of functionality. (See its source code for
//! details.)
//!
//! We then call [`task::run_tasks()`] and for the first time go to
//! userspace.

// #![deny(warnings)]
//#![deny(missing_docs)]
// #![allow(unused)]
// #![deny(warnings)]
#![allow(unused_must_use)]
#![allow(unreachable_code)]
#![allow(dead_code)]
#![allow(unused_imports)]
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(sync_unsafe_cell)]
extern crate alloc;

#[macro_use]
extern crate bitflags;

#[macro_use]
pub mod console;
pub mod arch;
pub mod config;
mod drivers;
pub mod fs;
pub mod lang_items;
pub mod logger;
pub mod mm;
#[cfg(feature = "net")]
pub mod net;
pub mod signal;
pub mod sync;
pub mod syscall;
pub mod task;
pub mod timer;
pub mod trap;
pub mod utils;

// use crate::{mm::activate_kernel_space};
use arch::*;
use cfg_if::cfg_if;
use core::{
    arch::asm,
    sync::atomic::{AtomicUsize, Ordering},
};
use log::info;
use smoltcp::phy::DeviceCapabilities;

use crate::drivers::{DeviceContainer, NetDeviceImpl};

/// clear BSS segment
fn clear_bss() {
    extern "C" {
        fn sbss();
        fn ebss();
    }
    unsafe {
        core::slice::from_raw_parts_mut(
            sbss as *const () as usize as *mut u8,
            ebss as *const () as usize - sbss as *const () as usize,
        )
        .fill(0);
    }
}

/// ADD KERNEL_ADDR_OFFSET and jump to rust_main
#[cfg(target_arch = "riscv64")]
#[no_mangle]
pub fn trampoline(hartid: usize) {
    unsafe {
        asm!("add sp, sp, {}", in(reg) arch::memory_layout::KERNEL_ADDR_OFFSET);
        asm!("la t0, rust_main");
        asm!("add t0, t0, {}", in(reg) arch::memory_layout::KERNEL_ADDR_OFFSET);
        asm!("mv a0, {}", in(reg) hartid);
        asm!("jalr zero, 0(t0)");
    }
}

// #[cfg(feature = "loongarch64")]
// #[no_mangle]
// pub fn trampoline(hartid: usize) {
//     unsafe {
//         asm!("add $sp, $sp, {}", in(reg) arch::memory_layout::KERNEL_ADDR_OFFSET);
//         asm!("la $t0, rust_main");
//         asm!("add $t0, $t0, {}", in(reg) arch::memory_layout::KERNEL_ADDR_OFFSET);
//         asm!("mv $a0, {}", in(reg) hartid);
//         asm!("jalr $zero, 0(t0)");
//     }
// }

const BOOT_UNINITIALIZED: usize = usize::MAX;
const BOOT_INITIALIZING: usize = 0;
const BOOT_ONLINE: usize = 1;

/// Startup state deliberately has a non-zero initial value, keeping it in
/// `.data` instead of the BSS that the bootstrap hart clears. A secondary hart
/// may enter the kernel before that clear has finished, so it must not observe
/// or modify a BSS-resident synchronisation flag.
static BOOT_STATE: AtomicUsize = AtomicUsize::new(BOOT_UNINITIALIZED);
static START_HART_ID: AtomicUsize = AtomicUsize::new(0);
// /// boot start_hart之外的所有 hart
// pub fn boot_all_harts(hartid: usize) {
//     for i in (0..arch::config::HART_NUM).filter(|id| *id != hartid) {
//         let sbi_ret = arch::cpu::hart_start(i, arch::memory_layout::HART_START_ADDR).into_result();
//         match sbi_ret {
//             Ok(_) => (), // 啥也不做
//             Err(_) => println!("Error when booting No.{} hart", i),
//         }
//     }
// }

#[no_mangle]
/// the rust entry-point of os
pub fn rust_main(hartid: usize) -> ! {
    let is_bootstrap = BOOT_STATE
        .compare_exchange(
            BOOT_UNINITIALIZED,
            BOOT_INITIALIZING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok();

    if is_bootstrap {
        clear_bss();
        println!("[kernel] Hello, world!");
        println!(
            r#"
                  ____                  
  _   _    __ _  |___ \   _   _    ___  
 | | | |  / _` |   __) | | | | |  / _ \ 
 | |_| | | (_| |  / __/  | |_| | | (_) |
  \__, |  \__,_| |_____|  \__, |  \___/ 
  |___/                   |___/         
            "#
        );
        #[cfg(target_arch = "loongarch64")]
        arch::memory_layout::print_memlayout();
        // 时钟频率初始化
        arch::time::init_clock_freq();
        // HXC: 我在这里加入了很多调试输出，因为是要提交给平台的，因此最好不要用debug!而是用print和println
        print!("mm::init...");
        mm::init();
        println!("complete.");

        print!("logger::init...");
        logger::init();
        println!("complete.");

        print!("trap::init...");
        trap::init();
        println!("complete.");

        print!("task::init...");
        task::init();
        println!("complete.");

        print!("fs::init...");
        fs::init();
        println!("complete.");

        print!("net::init...");
        #[cfg(all(feature = "net", target_arch = "loongarch64"))]
        let net_devices = DeviceContainer::from_one(NetDeviceImpl::new_device());
        #[cfg(all(feature = "net", target_arch = "riscv64"))]
        let net_devices = match NetDeviceImpl::try_new_device() {
            Some(device) => DeviceContainer::from_one(device),
            None => DeviceContainer::default(),
        };
        #[cfg(feature = "net")]
        net::init_network(net_devices);
        println!("complete.");

        print!("task::add_initproc...");
        task::add_initproc();
        println!("complete.");

        print!("START_HART_ID.store...");
        START_HART_ID.store(hartid, Ordering::Release);
        println!("complete.");

        #[cfg(target_arch = "riscv64")]
        {
            print!("boot secondary harts...");
            arch::cpu::boot_secondary_harts(hartid);
            println!("complete.");
        }

        print!("BOOT_STATE.store(ONLINE)...");
        BOOT_STATE.store(BOOT_ONLINE, Ordering::Release);
        println!("complete.");

        print!("trap::enable_timer_interrupt...");
        arch::trap_interface::enable_timer_interrupt();
        println!("complete.");

        print!("timer::set_next_trigger...");
        timer::set_next_trigger();
        println!("complete.");
    } else {
        // barrier
        while BOOT_STATE.load(Ordering::Acquire) != BOOT_ONLINE {
            core::hint::spin_loop();
        }

        println!(
            "[kernel] ---------- hart {} is starting... ----------",
            hartid
        );
        trap::init();
        mm::activate_kernel_space();
        arch::trap_interface::enable_timer_interrupt();
        timer::set_next_trigger();
    }
    if arch::cpu::hart_id() == START_HART_ID.load(Ordering::Acquire) {
        fs::list_apps();
    }
    task::run_tasks();
    panic!("Unreachable in rust_main!");
}
