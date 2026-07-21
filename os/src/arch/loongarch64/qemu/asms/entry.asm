    .section .text.entry
    .globl _start

.equ BOOT_STACK_SIZE, 0x40000
# Keep this in sync with crate::arch::config::HART_NUM.
.equ MAX_HARTS, 8

_start:
    # 清空 TLB
    invtlb      0x0, $zero, $zero
    
    # 初始化栈指针
    la          $sp, boot_stack
    # Early PCI VirtIO/net and filesystem initialization nests large Rust
    # frames before the scheduler switches to per-task kernel stacks.
    li.d        $a0, BOOT_STACK_SIZE
    csrrd       $a1, 0x20   # CPUID，这个id是如何编号的是硬件决定的（不一定从0开始编号）
    addi.d      $tp, $a1, 0
    addi.d      $a1, $a1, 1
    mul.d       $a0, $a0, $a1
    add.d       $sp, $sp, $a0

    bl          init_csr_regs
spin:
    b           spin

    .section .bss.stack
    .globl boot_stack
boot_stack:
    .space BOOT_STACK_SIZE * MAX_HARTS
