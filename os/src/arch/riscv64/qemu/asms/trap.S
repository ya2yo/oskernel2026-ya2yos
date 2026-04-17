.attribute arch, "rv64gc"

# ======偏移量定义======

# 下面保存的是TrapContext的各字段在结构体中的偏移量
# 单位：字节
TRAPCONTEXT_GP = 0                   # 通用寄存器组
TRAPCONTEXT_SP = TRAPCONTEXT_GP+2*8  # sp是gp的一部分
TRAPCONTEXT_SSTATUS = 32*8
TRAPCONTEXT_SEPC = 33*8
TRAPCONTEXT_KERNEL_STACK = 34*8
TRAPCONTEXT_HARTID = 35*8
TRAPCONTEXT_ORIGIN_A0 = 36*8
TRAPCONTEXT_FP = 37*8
TRAPCONTEXT_FCSR = TRAPCONTEXT_FP+32*8 # fcsr是fp的一部分


# ======宏定义======

.altmacro
.macro SAVE_GP n
    sd x\n, (\n*8+TRAPCONTEXT_GP)(sp)
.endm
.macro LOAD_GP n
    ld x\n, (\n*8+TRAPCONTEXT_GP)(sp)
.endm
.macro SAVE_FP n
    fsd f\n, (\n*8+TRAPCONTEXT_FP)(sp)
.endm
.macro LOAD_FP n
    fld f\n, (\n*8+TRAPCONTEXT_FP)(sp)
.endm

.macro SAVE_GPS_EXCEPT_SP
    SAVE_GP 1
    .set n, 3
    .rept 29
        SAVE_GP %n
        .set n, n+1
    .endr
.endm

.macro LOAD_GPS_EXCEPT_SP
    LOAD_GP 1
    .set n, 3
    .rept 29
        LOAD_GP %n
        .set n, n+1
    .endr
.endm

# 保存浮点寄存器，不含fcsr
.macro SAVE_FPS
    .set n, 0
    .rept 32
        SAVE_FP %n
        .set n, n+1
    .endr
.endm

.macro LOAD_FPS
    .set n, 0
    .rept 32
        LOAD_FP %n
        .set n, n+1
    .endr
.endm

# ======浮点开关======

# 如果设成1，则一切正常；
# 如果设成0，则不会保存和恢复fp寄存器以及fcsr
.set ENABLE_FPU, 0

# ======正文======

.section .text
.globl __trap_from_user
.globl __return_to_user
.align 2

# 声明外部函数
.extern trap_entry

# 用户态代码陷入中断后会转到进入此处
# 此函数的入口即stvec的值
__trap_from_user:
    # 从sscratch中取出TrapContext指针到sp，将原来的用户态sp暂时地存入sscratch
    csrrw sp, sscratch, sp
    # 保存除了sp之外的通用寄存器到TrapContext中，从x1到x31，但是没有x2(即sp)
    SAVE_GPS_EXCEPT_SP

    .if ENABLE_FPU == 1

        # 保存所有的浮点寄存器到TrapContext中
        SAVE_FPS
        # 保存fcsr
        csrr t0, fcsr
        sd t0, TRAPCONTEXT_FCSR(sp)
    .endif


    # 保存用户态a0寄存器的值（有什么意义？）
    sd a0, TRAPCONTEXT_ORIGIN_A0(sp)
    # 保存sstatus, sepc到TrapContext中
    csrr t0, sstatus
    csrr t1, sepc
    sd t0, TRAPCONTEXT_SSTATUS(sp)
    sd t1, TRAPCONTEXT_SEPC(sp)
    # 保存用户态sp（它现在存在sscratch中）
    csrr t2, sscratch
    sd t2, TRAPCONTEXT_SP(sp)
    # 转到内核控制流
    mv s0, zero     # s0/fp是帧指针，初始化成0比较好吧
    ld tp, TRAPCONTEXT_HARTID(sp)
    ld sp, TRAPCONTEXT_KERNEL_STACK(sp)
    j trap_entry

# kernel -> user
__return_to_user:
    # a0: *TrapContext in user space(Constant); a1: user space token
    # switch to user space

    # let sscratch store the trap context's address
    csrw sscratch, a0
    # 在remove_trap_loop这一更改后，这里不应该保存各寄存器了
    # 如果保存sp，会导致栈溢出！
    sd tp, TRAPCONTEXT_HARTID(a0)
    mv sp, a0
    # now sp points to TrapContext in kernel space, start restoring based on it
    # restore sstatus/sepc/fcsr
    
    ld t0, TRAPCONTEXT_SSTATUS(sp)
    csrw sstatus, t0
    ld t0, TRAPCONTEXT_SEPC(sp)
    csrw sepc, t0

    
    .if ENABLE_FPU == 1
        LOAD_FPS
        ld t0, TRAPCONTEXT_FCSR(sp)
        csrw fcsr, t0
    .endif

    # restore general purpose registers except x0/sp/
    LOAD_GPS_EXCEPT_SP

    # back to user stack
    ld sp, TRAPCONTEXT_SP(sp)
    sret