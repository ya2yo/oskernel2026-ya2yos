        .globl __tlb_rfill
        .section .text.__rfill
        .balign 4096
__tlb_rfill:
        csrwr $t0, 0x8B
        csrrd $t0, 0x1B
        lddir $t0, $t0, 2
        lddir $t0, $t0, 1
        ldpte $t0, 0
        #取回偶数号页表项
        ldpte $t0, 1
        #取回奇数号页表项
        tlbfill
        csrrd $t0, 0x8B
        #jr $ra
        ertn