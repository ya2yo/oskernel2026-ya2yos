//! Loongson 2K1000 reference-board platform support.
//!
//! The board has a different load address, UART, power controller, and SATA
//! controller from QEMU `virt`. Trap, page-table, context, timer, and user
//! access code are shared with the LoongArch platform implementation.

mod asms;
pub mod config;
#[path = "../qemu/console.rs"]
pub mod console;
#[path = "../qemu/context/mod.rs"]
pub mod context;
pub mod cpu;
pub mod memory_layout;
#[path = "../qemu/page_table.rs"]
pub mod page_table;
#[path = "../qemu/time.rs"]
pub mod time;
#[path = "../qemu/tlb.rs"]
pub mod tlb;
#[path = "../qemu/trap_interface.rs"]
pub mod trap_interface;
#[path = "../qemu/uaccess.rs"]
pub mod uaccess;
