# 将initproc提前放到内存里
    .section .data
    .global initproc_start
    .global initproc_end
    .align 12
initproc_start:
    .incbin "../user/target/loongarch64-unknown-none/release/initproc"
initproc_end:
    .align 12