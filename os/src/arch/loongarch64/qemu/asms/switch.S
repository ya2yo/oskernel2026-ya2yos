.altmacro
.macro SAVE_SN n
    st.d $s\n, $a0, (\n+2)*8
.endm
.macro LOAD_SN n
    ld.d $s\n, $a1, (\n+2)*8
.endm
    .section .text
    .globl __switch
__switch:
    # __switch(
    #     current_task_cx_ptr: *mut TaskContext,
    #     next_task_cx_ptr: *const TaskContext
    # )
    # save kernel stack of current task
    st.d $sp, $a0, 8
    # save ra & s0~s8 of current execution
    st.d $ra, $a0, 0
    .set n, 0
    .rept 9
        SAVE_SN %n
        .set n, n + 1
    .endr
    st.d $fp, $a0, (9+2)*8
    # restore ra & s0~s8 of next execution
    ld.d $ra, $a1, 0
    .set n, 0
    .rept 9
        LOAD_SN %n
        .set n, n + 1
    .endr
    ld.d $fp, $a1, 0x58
    # restore kernel stack of next task
    ld.d $sp, $a1, 8
    li.d $a0, 0 # 告诉跳转目的位置，这是__switch而不是abandon的结果
    jr $ra

.section .text
.globl __abandon
__abandon:
    # 由HXC创建
    # 该函数的作用是
    # 将a1的状态写入到CPU上。CPU原先的状态将被丢弃
    # 同时，不改变a0的值
    # 第一个参数会作为返回值被返回回去
    ld.d $ra, $a1, 0
    .set n, 0
    .rept 9
        LOAD_SN %n
        .set n, n + 1
    .endr
    ld.d $fp, $a1, 0x58
    ld.d $sp, $a1, 8
    jr $ra

