.attribute arch, "rv64gc"

.section .text
.align 2

.globl __ya2y_raw_copy_from_user
.globl __ya2y_raw_copy_to_user
.globl __ya2y_uaccess_fault_fixup

# a0: source/destination pointer
# a1: destination/source pointer
# a2: byte length
# a0 return value: 0 on success, 1 after the fault fixup path
__ya2y_raw_copy_from_user:
    beqz a2, .Lya2y_copy_from_done
.Lya2y_copy_from_loop:
    lbu t0, 0(a0)
    sb t0, 0(a1)
    addi a0, a0, 1
    addi a1, a1, 1
    addi a2, a2, -1
    bnez a2, .Lya2y_copy_from_loop
.Lya2y_copy_from_done:
    li a0, 0
    ret

__ya2y_raw_copy_to_user:
    beqz a2, .Lya2y_copy_to_done
.Lya2y_copy_to_loop:
    lbu t0, 0(a0)
    sb t0, 0(a1)
    addi a0, a0, 1
    addi a1, a1, 1
    addi a2, a2, -1
    bnez a2, .Lya2y_copy_to_loop
.Lya2y_copy_to_done:
    li a0, 0
    ret

__ya2y_uaccess_fault_fixup:
    li a0, 1
    ret
