use core::arch::global_asm;

global_asm!(include_str!("trap.S"));
global_asm!(include_str!("entry.asm"));
global_asm!(include_str!("preload.S"));
global_asm!(include_str!("switch.S"));
global_asm!(include_str!("tlb.S"));
