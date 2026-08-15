    # Keep this resource capacity in sync with hardware::MAX_SUPPORTED_HARTS.
    # path mounts the filesystem and initializes the network before switching
    # to dynamically allocated kernel stacks, so 64 KiB can overflow when
    # hart 1 wins the bootstrap race and overwrite adjacent .data.
    .equ BOOT_STACK_SHIFT, 17
    .equ BOOT_STACK_SIZE, (1 << BOOT_STACK_SHIFT)
    .equ BOOT_HARTS, 16

    .section .text.entry
    .globl _start
_start:
    # rust sbi put hart id on a0
    # alloc kernel stack for each hart
    # set sp(each hart has one kstack)
    mv tp,a0
    slli t0, a0, BOOT_STACK_SHIFT
    la sp, boot_stack_top
    sub sp, sp, t0  # sp = stack top - hart_id * stack_size

    # The kernel is linked in the high half, so activate the bootstrap page
    # table before jumping to Rust.  QEMU 10 may place the SBI FDT near the
    # end of guest RAM; map its 1 GiB physical leaf before paging so
    # hardware::init_from_fdt() can read a1 immediately after this switch.
    # satp: 8 << 60 | boot_pagetable
    la t0, boot_pagetable
    srli t1, a1, 30
    li t2, 512
    bgeu t1, t2, .Lbootstrap_fdt_map_done
    slli t2, t1, 3
    add t2, t0, t2
    slli t1, t1, 28
    ori t1, t1, 0xcf # VRWXAD 1 GiB identity leaf
    sd t1, 0(t2)
.Lbootstrap_fdt_map_done:
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
    # Keep the kernel's first GiB identity and high-half aliases.  The FDT
    # leaf is installed dynamically above because firmware may move it.
    # 0x0000_0000_8000_0000 -> 0x0000_0000_8000_0000
    # 0xffff_fc00_8000_0000 -> 0x0000_0000_8000_0000
    .quad 0
    .quad 0
    .quad (0x80000 << 10) | 0xcf # VRWXAD
    .zero 8 * 255
    .quad (0x80000 << 10) | 0xcf # VRWXAD
    .zero 8 * 253

    .section .text.trampoline
    .align 12
    .global sigreturn_trampoline
sigreturn_trampoline:
    li	a7,139
    ecall
