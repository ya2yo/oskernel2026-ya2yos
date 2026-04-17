    .section .text.entry
    .globl _start
_start:
    # 清空 TLB
    invtlb      0x0, $zero, $zero
    
    # 初始化栈指针
    la          $sp, boot_stack
    li.d        $a0, 4096
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
    .space 4096 * 16  # 这里为每个CPU预留一个页，最多支持16个CPU（暂时）
