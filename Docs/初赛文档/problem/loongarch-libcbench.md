# 龙芯架构 libcbench_testcode

龙芯下用户态非法指令导致内核死循环的问题。
主要改动：

- os/src/arch/loongarch64/qemu/trap_interface.rs:
  - tlb_page_modify_handler() 改为用 badv::read().vaddr()
  - 保留之前把 InstructionNotExist 映射为 IllegalInstruction 的修复
- os/src/trap/mod.rs:
  - 启用非法指令处理
  - timer 中断分支内立即重装下一次 timer
- os/src/timer.rs:
  - 恢复 TICKS_PER_SEC 频率，不再 1 秒一次 tick
- os/src/trap/trap_types.rs:
  - 新增 IllegalInstruction
