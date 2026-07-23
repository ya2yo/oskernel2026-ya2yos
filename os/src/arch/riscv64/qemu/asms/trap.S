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
    # ---------------------------------------------------------------------
    # 1. 关键栈指针切换 (The Magic Switch)
    # ---------------------------------------------------------------------
    # csrrw (Atomic Read/Write CSR): 
    # 将 sp (当前用户栈指针) 写入 sscratch，同时将 sscratch (预先存好的内核TrapContext指针) 读入 sp。
    # 
    # 执行前: 
    #   sp = 用户栈顶
    #   sscratch = 内核态为该线程准备的 TrapContext 地址 (在 return_to_user 时设置)
    #
    # 执行后: 
    #   sp = TrapContext 地址 (现在 sp 指向内核空间的安全区域，后续保存操作都在这里进行)
    #   sscratch = 原来的用户栈 sp (暂存用户栈，稍后保存)
    # ---------------------------------------------------------------------
    csrrw sp, sscratch, sp
    # ---------------------------------------------------------------------
    # 2. 保存通用寄存器
    # ---------------------------------------------------------------------
    # 宏 SAVE_GPS_EXCEPT_SP 会将 x1 到 x31 的所有通用寄存器保存到 sp 指向的内存 (TrapContext) 中。
    # 这里不保存 x2 (sp)，因为刚才 sp 已经被交换成了 TrapContext 的地址，而不是用户原本的 sp。
    # 用户原本的 sp 此时正躺在 sscratch 寄存器里。
    SAVE_GPS_EXCEPT_SP
    # ---------------------------------------------------------------------
    # 3. 保存浮点寄存器 (如果启用)
    # ---------------------------------------------------------------------
    .if ENABLE_FPU == 1
        # 保存所有浮点寄存器 (f0-f31) 到 TrapContext
        SAVE_FPS
        # 读取浮点控制状态寄存器 (fcsr)，包含舍入模式和异常标志
        csrr t0, fcsr
         # 保存到 TrapContext 结构体中
        sd t0, TRAPCONTEXT_FCSR(sp)
    .endif

    # ---------------------------------------------------------------------
    # 4. 保存关键状态寄存器
    # ---------------------------------------------------------------------

    # 保存用户态a0寄存器的值（有什么意义？）
    # 在系统调用（ecall）的 ABI 中，a0 通常用于存放系统调用号（进入时）和 返回值（退出时）。
    # 此外，如果发生页错误或其他异常，内核可能需要检查触发异常的那条指令的操作数（往往在 a0 中）。
    # 这里保存一份原始副本，是为了防止内核在处理中断的过程中覆盖了 a0，导致返回用户态时数据出错，
    # 或者为了调试/分析异常原因而保留现场。
    sd a0, TRAPCONTEXT_ORIGIN_A0(sp)
    # 保存sstatus, sepc到TrapContext中
    csrr t0, sstatus
    csrr t1, sepc
    sd t0, TRAPCONTEXT_SSTATUS(sp)
    sd t1, TRAPCONTEXT_SEPC(sp)
    # ---------------------------------------------------------------------
    # 5. 恢复并保存用户栈指针
    # ---------------------------------------------------------------------
    # 此时 sscratch 中存放的是第 1 步交换进来的“用户态 sp”。
    # 我们将其读出到 t2
    csrr t2, sscratch
    # 将这个用户态的 sp 保存到 TrapContext 的 sp 字段中。
    # 这样，当以后要从内核返回用户态时，就能从这里把 sp 还原回去。
    sd t2, TRAPCONTEXT_SP(sp)
    # ---------------------------------------------------------------------
    # 6. 切换到内核控制流
    # ---------------------------------------------------------------------
    # 初始化帧指针 s0/fp 为 0，表示没有调用帧，防止内核回溯栈时出错
    mv s0, zero     # s0/fp是帧指针，初始化成0比较好吧
    # 从 TrapContext 中取出 Hart ID (核心ID)，恢复到 tp 寄存器
    # (tp 通常在系统启动时初始化，指向核心本地数据)
    ld tp, TRAPCONTEXT_HARTID(sp)
    # 【关键步骤】切换栈指针到真正的内核栈
    # 目前 sp 还指向 TrapContext (通常在内核堆或进程控制块中)。
    # 这里从 TrapContext 中读取预先设置好的“内核栈顶地址”，赋值给 sp。
    # 从此以后，push/pop 操作都在内核栈上进行，TrapContext 被留在了身后。
    ld sp, TRAPCONTEXT_KERNEL_STACK(sp)
    j trap_entry

# kernel -> user
__return_to_user:
    # -----------------------------------------------------------------------
    # 参数说明 (RISC-V 调用约定):
    # a0: 指向用户空间 TrapContext 的指针 (这是物理地址或者映射后的内核虚拟地址)
    # a1: 用户空间的页表令牌 (通常用于地址空间切换，虽然本段代码未直接使用，但隐含在上下文切换中)
    # -----------------------------------------------------------------------

    # 1. 设置 sscratch
    # sscratch 寄存器在内核中通常用来快速保存内核栈指针或 Trap 上下文指针。
    # 这里将 a0 (用户 TrapContext 地址) 写入 sscratch，以便在下次陷入内核时汇编代码能立即找到上下文保存的位置
    csrw sscratch, a0
    # 2. 保存 Hart ID (线程ID)
    # 注释提到 "在remove_trap_loop这一更改后..."，说明代码经过优化。
    # 这里将 tp 寄存器 (通常存放 Hart ID 或线程本地存储指针) 保存到 TrapContext 的特定偏移处。
    # 这是为了在下次进入内核时，能立刻知道是哪个核心/线程触发的中断。
    sd tp, TRAPCONTEXT_HARTID(a0)
    # 3. 切换栈指针
    # 将 sp (栈指针) 指向 a0 (TrapContext)。
    # 这意味着接下来的 ld (load) 操作将直接从 TrapContext 结构体中读取数据，而不是从传统的函数调用栈中读取
    mv sp, a0

    # 4. 恢复 CSR (控制与状态寄存器)
    # TrapContext 中保存了陷入内核前的 sstatus 和 sepc，现在将它们写回硬件寄存器。

    # 恢复 sstatus (超级用户状态寄存器)
    # 包括中断使能位(SIE)、之前的特权模式(SPP)等信息。
    ld t0, TRAPCONTEXT_SSTATUS(sp)
    csrw sstatus, t0
    # 恢复 sepc
    # 这决定了 sret 执行后跳转回哪里(即用户程序下一条要执行的指令地址)。
    ld t0, TRAPCONTEXT_SEPC(sp)
    csrw sepc, t0

    # 5. 恢复浮点单元 (FPU) 上下文 
    .if ENABLE_FPU == 1
        LOAD_FPS
        ld t0, TRAPCONTEXT_FCSR(sp)
        csrw fcsr, t0
    .endif

    # 6. 恢复通用寄存器
    # LOAD_GPS_EXCEPT_SP 是一个宏，用于恢复 x1-x31 中除了 sp 以外的所有通用寄存器。
    # 为什么排除 sp？因为 sp (栈指针) 需要在最后单独处理，或者它是切换地址空间的关键。
    LOAD_GPS_EXCEPT_SP

    # 7. 切换回用户栈
    # 此时 sp 仍然指向 TrapContext (位于内核栈或内核映射区)。
    # 这里从 TrapContext 中读取之前保存的用户栈指针 (用户空间的 sp 值)。
    # 执行后，sp 指向了用户进程的栈顶。
    ld sp, TRAPCONTEXT_SP(sp)
    # 8. 返回用户态
    # sret 指令执行以下操作：
    # 1. 将程序计数器 (PC) 设置为 sepc 寄存器的值 (即跳转回用户代码)。
    # 2. 根据 sstatus 中的 SPP 位，将特权级从 S 模式 (内核) 降为 U 模式 (用户)。
    # 3. 更新 sstatus 中的中断使能位。
    sret
