.altmacro
.macro SAVE_SN n
    sd s\n, (\n+2)*8(a0)
.endm
.macro LOAD_SN n
    ld s\n, (\n+2)*8(a1)
.endm
    .section .text
    .globl __switch
__switch:
    # save kernel stack of current task
    sd sp, 8(a0)
    # save ra & s0~s11 of current execution
    sd ra, 0(a0)
    .set n, 0
    .rept 12
        SAVE_SN %n
        .set n, n + 1
    .endr
    # restore ra & s0~s11 of next execution
    ld ra, 0(a1)
    .set n, 0
    .rept 12
        LOAD_SN %n
        .set n, n + 1
    .endr
    # restore kernel stack of next task
    ld sp, 8(a1)
    # 将a0设置为0以告诉调用者：这是从switch来的，不需要free stack
    li a0, 0
    ret

.section .text
.globl __abandon
__abandon:
    # 由HXC创建
    # 该函数的作用是
    # 将a1的状态写入到CPU上。CPU原先的状态将被丢弃
    # 同时，不改变a0的值
    # 第一个参数会作为返回值被返回回去
    ld ra, 0(a1)
    .set n, 0
    .rept 12
        LOAD_SN %n
        .set n, n + 1
    .endr
    ld sp, 8(a1)
    ret
