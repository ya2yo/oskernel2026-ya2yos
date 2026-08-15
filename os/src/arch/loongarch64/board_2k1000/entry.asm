    .section .text.entry
    .globl _start

.equ BOOT_STACK_SIZE, 0x40000
.equ MAX_HARTS, 2

_start:
    invtlb      0x0, $zero, $zero

    la          $sp, boot_stack
    li.d        $a0, BOOT_STACK_SIZE
    csrrd       $a1, 0x20
    addi.d      $tp, $a1, 0
    addi.d      $a1, $a1, 1
    mul.d       $a0, $a0, $a1
    add.d       $sp, $sp, $a0

    addi.d      $a0, $a2, 0 # EFI system-table offset, per LoongArch boot ABI
    bl          init_csr_regs
spin:
    b           spin

    .section .bss.stack
    .globl boot_stack
boot_stack:
    .space BOOT_STACK_SIZE * MAX_HARTS
