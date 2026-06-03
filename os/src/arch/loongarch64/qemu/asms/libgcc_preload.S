# 将 libgcc_s.so.1 内嵌到内核中，启动时写入文件系统
# LoongArch 版本，路径可能需根据实际评测机调整
    .section .data
    .global libgcc_s_start
    .global libgcc_s_end
    .align 12
libgcc_s_start:
    .incbin "/opt/gcc-13.2.0-loongarch64-linux-gnu/loongarch64-linux-gnu/lib64/libgcc_s.so.1"
libgcc_s_end:
    .align 12
