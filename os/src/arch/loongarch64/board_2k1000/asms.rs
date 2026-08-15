use core::arch::global_asm;

global_asm!(include_str!("../qemu/asms/trap.S"));
global_asm!(include_str!("entry.asm"));
global_asm!(include_str!("../qemu/asms/preload.S"));
global_asm!(include_str!("../qemu/asms/switch.S"));
global_asm!(include_str!("../qemu/asms/tlb.S"));
global_asm!(include_str!("../qemu/asms/libgcc_preload.S"));
global_asm!(include_str!("../qemu/asms/uaccess.S"));
