# 将 libgcc_s.so.1 内嵌到内核中，启动时写入文件系统
# 评测机 Docker 环境中该文件位于 /usr/riscv64-linux-gnu/lib/libgcc_s.so.1
    .section .data
    .global libgcc_s_start
    .global libgcc_s_end
    .align 12
libgcc_s_start:
    .incbin "/usr/riscv64-linux-gnu/lib/libgcc_s.so.1"
libgcc_s_end:
    .align 12
