# 嵌套 LoongArch64 QEMU 的 EFI 启动流程

## 背景

final-2026 的 LoongArch64 镜像包含 BuildStorm 工具链和 `arceos-helloworld` 构建环境。
`initproc` 需要在构建完成后启动该产物，验证嵌套 QEMU 能执行 LoongArch64 版本的
ArceOS hello world。

## 分析

镜像内脚本使用以下约定：

- 构建产物为 `/work/tgoskits/target/loongarch64-unknown-linux-musl/release/arceos-helloworld`；
- `.bin` 文件作为 EFI 应用复制到 `EFI/BOOT/BOOTLOONGARCH64.EFI`；
- QEMU 根目录为 `/opt/qemu-la64`，包含 LoongArch64 动态加载器、QEMU 和 EDK2 固件；
- QEMU 使用 `virt`、`la464`、单核、2 GiB 内存和两个 pflash 驱动，再挂载 FAT ESP。

因此不能直接复用 RISC-V 的 `-kernel` 命令，否则无法进入镜像要求的 EFI 启动路径。

## 修复

在 `user/src/bin/initproc.rs` 中新增仅对 `loongarch64` 编译的
`boot_arceos_helloworld_in_qemu`：子进程先准备 FAT ESP 和变量固件副本，再通过
`/opt/qemu-la64/lib/ld-linux-loongarch-lp64d.so.1` 执行 QEMU。RISC-V 实现及其路径不变，
final 测试入口根据目标架构选择对应实现。

## 涉及文件

- `user/src/bin/initproc.rs`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

已使用 `file`、`dumpe2fs` 和 `strings` 对 `2026_testsuits_img/final-2026/sdcard-la.img`
进行只读检查，并核对镜像内脚本的 LoongArch64 EFI/QEMU 参数。当前宿主缺少镜像挂载所需的
loop 设备，且本轮尚未运行 LoongArch64 QEMU 或完整双架构构建。
