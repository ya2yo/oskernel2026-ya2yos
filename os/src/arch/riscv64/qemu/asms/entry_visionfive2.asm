    # VisionFive 2 enters the kernel at physical 0x40000000. Keep an
    # identity mapping while enabling Sv39, then jump through the high-half
    # alias used by the Rust kernel.
    .equ BOOT_STACK_SHIFT, 17
    .equ BOOT_STACK_SIZE, (1 << BOOT_STACK_SHIFT)
    .equ BOOT_HARTS, 16
    .equ KERNEL_OFFSET, 0xffffffc040000000

    .section .text.entry
    .globl _start
_start:
    mv tp, a0
    slli t0, a0, BOOT_STACK_SHIFT
    la sp, boot_stack_top
    sub sp, sp, t0

    la t0, boot_pagetable
    li t1, 8 << 60
    srli t0, t0, 12
    or t0, t0, t1
    csrw satp, t0
    sfence.vma

    call trampoline

    .section .bss.stack
    .globl boot_stack_lower_bound
boot_stack_lower_bound:
    .space BOOT_STACK_SIZE * BOOT_HARTS
    .globl boot_stack_top
boot_stack_top:

    .section .data
    .align 12
boot_pagetable:
    # Root entry 1: 0x40000000 -> 0x40000000 (identity, 1 GiB)
    .zero 8
    .quad (0x10000 << 10) | 0xcf
    .zero 8 * 254

    .section .text.trampoline
    .align 12
    .global sigreturn_trampoline
sigreturn_trampoline:
    li a7, 139
    ecall
    # Root entry 257: 0xffffffc040000000 -> 0x40000000 (high alias)
    .zero 8 * 1
    .quad (0x10000 << 10) | 0xcf
    .zero 8 * 254
