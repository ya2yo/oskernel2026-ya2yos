    .section .text.entry
    .globl _start

.equ BOOT_STACK_SIZE, 0x40000
.equ MAX_HARTS, 2

_start:
    # Direct UART markers for raw U-Boot `go` bring-up. They avoid the Rust
    # console locks and show whether execution reaches Rust initialization.
    li.d        $t3, 0x900000001fe20000
    li.w        $t4, 0x41 # A
    st.b        $t4, $t3, 0

    # U-Boot's `go` enters an application as a normal C function:
    # a0 = argc, a1 = argv.  Preserve those values while preparing the
    # per-hart bootstrap stack; a2 remains available for an EFI handoff.
    addi.d      $t0, $a0, 0
    addi.d      $t1, $a1, 0
    addi.d      $t2, $a2, 0

    la          $sp, boot_stack
    li.d        $a0, BOOT_STACK_SIZE
    csrrd       $a1, 0x20
    addi.d      $tp, $a1, 0
    addi.d      $a1, $a1, 1
    mul.d       $a0, $a0, $a1
    add.d       $sp, $sp, $a0

    addi.d      $a0, $t0, 0
    addi.d      $a1, $t1, 0
    addi.d      $a2, $t2, 0
    li.d        $t3, 0x900000001fe20000
    li.w        $t4, 0x42 # B
    st.b        $t4, $t3, 0
    bl          init_csr_regs
spin:
    b           spin

    .section .bss.stack
    .globl boot_stack
boot_stack:
    .space BOOT_STACK_SIZE * MAX_HARTS
