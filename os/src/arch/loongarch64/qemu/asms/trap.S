LSX_START = 16
.altmacro
.macro SAVE_GP n
    st.d $r\n, $sp, \n*8
.endm
.macro LOAD_GP n
    ld.d $r\n, $sp, \n*8
.endm
.macro SAVE_LSX n, m
    vst $vr\n, $sp, \m*16
.endm
.macro LOAD_LSX n, m
    vld $vr\n, $sp, \m*16
.endm
    .section .text.trampoline
    .globl __alltraps
    .globl __return_to_user
    .align 2

.equ CSR_SAVE0, 0x30    
.equ CSR_SAVE1, 0x31
.equ CSR_SAVE2, 0x32
.equ CSR_SAVE3, 0x33
.equ CSR_SAVE4, 0x34
.equ CSR_SAVE5, 0x35
.equ CSR_SAVE6, 0x36
.equ CSR_SAVE7, 0x37

.equ CSR_ERA, 0x6
.equ CSR_PRMD, 0x1
.equ CSR_PGDL, 0x19

.extern trap_entry


__alltraps:
    #==================REMEMBER TO TURN OFF THE INTERRUPT !=======================
    # 此时的sp应该是用户态sp，如果它是某几个特殊值，则shutdown

    csrwr $sp, CSR_SAVE0
    # now sp->*TrapContext in user space, CSR_SAVE0->user stack

    # 考虑到此时sp理论上应当是TrapContext，我们应当对sp检查是否为页对齐
    # 先将sp存到CSR_SAVE4中
    #csrwr $sp, CSR_SAVE4
    #csrrd $sp, CSR_SAVE4
    #andi $sp, $sp, 0xFFF
    #bnez $sp, __restore_sp
    #csrrd $sp, CSR_SAVE4

    SAVE_GP 1
    SAVE_GP 2

    # skip r3(sp)
    .set n, 4
    .rept 28
        SAVE_GP %n
        .set n, n+1
    .endr
    .set n, 0

    # GeneralRegs is 32 x 8 bytes.  FloatRegs then stores 32 x 128-bit
    # LSX registers, followed by FCSR/FCC; keep these offsets in sync with
    # context/regs.rs and context/trap_context.rs.
    .set m, LSX_START
    .rept 32
        SAVE_LSX %n, %m
        .set n, n+1
        .set m, m+1
    .endr

    # we can use t0/t1/t2 freely, because they have been saved in TrapContext
    # save FCSR
    movfcsr2gr $t0, $fcsr0
    st.w $t0, $sp, 96*8

    # save all eight FCC bits in one byte.  Each movcf2gr overwrites its
    # destination, so shifting without accumulating would preserve fcc0 only.
    move $t1, $zero
    movcf2gr $t0, $fcc0
    or $t1, $t1, $t0
    movcf2gr $t0, $fcc1
    slli.d $t0, $t0, 1
    or $t1, $t1, $t0
    movcf2gr $t0, $fcc2
    slli.d $t0, $t0, 2
    or $t1, $t1, $t0
    movcf2gr $t0, $fcc3
    slli.d $t0, $t0, 3
    or $t1, $t1, $t0
    movcf2gr $t0, $fcc4
    slli.d $t0, $t0, 4
    or $t1, $t1, $t0
    movcf2gr $t0, $fcc5
    slli.d $t0, $t0, 5
    or $t1, $t1, $t0
    movcf2gr $t0, $fcc6
    slli.d $t0, $t0, 6
    or $t1, $t1, $t0
    movcf2gr $t0, $fcc7
    slli.d $t0, $t0, 7
    or $t1, $t1, $t0
    st.b $t1, $sp, 96*8+4

    # Save the user trap context fields.  __alltraps runs with the user
    # TrapContext as its stack, so the origin-a0 and PRMD slots are part of the
    # full context.  The separate __kern_trap entry below uses a 256-byte
    # kernel-only frame and deliberately does not write these slots.
    st.d $a0, $sp, 98*8
    csrrd $t0, CSR_PRMD
    st.d $t0, $sp, 99*8
    csrrd $t0, CSR_ERA
    st.d $t0, $sp, 0
    # read user stack from SAVE and save it in TrapContext
    csrrd $t2, CSR_SAVE0
    st.d $t2, $sp, 3*8
    # move to kernel_sp
    ld.d $sp, $sp, 100*8
    invtlb 0x0, $zero, $zero
    bl trap_entry   # 应该前往trap_entry，而后者是一个常数

__return_to_user:
    # a0: *TrapContext in user space(Constant), 
    invtlb 0x0, $zero, $zero
    move $sp, $a0
    csrwr  $a0, CSR_SAVE0
    # now sp points to TrapContext in user space, start restoring based on it
    # restore FCSR
    ld.w $t0, $sp, 96*8
    movgr2fcsr $fcsr0, $t0
    # restore FCC
    ld.b $t0, $sp, 96*8+4
    movgr2cf $fcc0, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc1, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc2, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc3, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc4, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc5, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc6, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc7, $t0
    # restore CSR_PRMD/CSR_ERA
    ld.d $t0, $sp, 99*8
    ld.d $t1, $sp, 0
    csrwr $t0, CSR_PRMD
    csrwr $t1, CSR_ERA
    # restore general purpose registers except r0/$sp
    LOAD_GP 1
    LOAD_GP 2
    .set n, 4
    .rept 28
        LOAD_GP %n
        .set n, n+1
    .endr
    .set n, 0
    .set m, LSX_START
    .rept 32
        LOAD_LSX %n, %m
        .set n, n+1
        .set m, m+1
    .endr
    # back to user stack
    LOAD_GP 3
    ertn

    .section .text.trampoline
    .globl sigreturn_trampoline
    .align 12
sigreturn_trampoline:
    # ecall sys_sigreturn 用户态信号处理结束后调用
    addi.d	$a7, $zero, 139
    syscall 0

    # The BuildStorm glibc uses 0xffff_ffff_fffe_4c44 as a normal indirect
    # helper call and expects it to return.  Keep that ABI entry distinct from
    # sigreturn_trampoline and report ENOSYS so the caller can take its regular
    # syscall fallback path.
    .space 0xc3c
    .globl legacy_vdso_fallback
legacy_vdso_fallback:
    addi.w  $a0, $zero, -38
    jirl    $zero, $ra, 0

# Kernel Trap Section
    .section .text.kern_trap
    .globl __kern_trap
    .align 12
__kern_trap:
    # Keep the original $sp in SAVE
    csrwr $sp, CSR_SAVE1
    csrrd $sp, CSR_SAVE1
    # Now move the $sp lower to push the registers
    addi.d $sp, $sp, -256
    # Align the $sp
    srli.d  $sp, $sp, 3
    slli.d  $sp, $sp, 3
    # now sp->*GeneralRegisters in kern space, CSR_SAVE1->(the previous $sp)

    SAVE_GP 1 # Save $ra
    SAVE_GP 2 # Save $tp

    # skip r3(sp)
    .set n, 4
    .rept 28
        SAVE_GP %n
        .set n, n+1
    .endr
    .set n, 0
    csrrd $t0, CSR_ERA
    st.d $t0, $sp, 0

    move $a0, $sp
    csrrd $sp, CSR_SAVE1
    st.d $sp, $a0, 3*8
    move $sp, $a0

    bl trap_from_kernel

    ld.d  $ra, $sp, 0
    csrwr $ra, CSR_ERA
    LOAD_GP 1
    LOAD_GP 2

    # skip r3(sp)
    .set n, 4
    .rept 28
        LOAD_GP %n
        .set n, n+1
    .endr
    .set n, 0
    
    csrrd $sp, CSR_SAVE1
    ertn

__restore_sp:
    csrrd $sp, CSR_SAVE4
__shutdown:
    lu12i.w         $r4, 65760
    ori             $r4, $r4, 28
    lu52i.d         $r4, $r4, -1792
    ori             $r5, $r0, 52
    st.b            $r5, $r4, 0
    __shutdown_loop:
    b               __shutdown_loop
