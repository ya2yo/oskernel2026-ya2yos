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

## 2026-08-14 后续：auxv HWCAP 与 QEMU guest RAM 映射

### 现象

新的 `server.ans` 已进入 Ya2yOS 的 `initproc`，但在启动内层 `/opt/qemu-la64` 的
`qemu-system-loongarch64` 前退出：

```text
TCG: unaligned access support required; exiting
```

补齐该能力位后，内层 QEMU 继续执行到 RAM 初始化，却报告：

```text
cannot set up guest memory 'loongarch.ram': Cannot allocate memory
```

### 根因

ELF loader 始终把 `AT_HWCAP` 写为零。QEMU LoongArch TCG 后端通过
`getauxval(AT_HWCAP)` 检查 Linux ABI 的 `HWCAP_LOONGARCH_UAL`（bit 2），缺少该位时
拒绝运行，以避免在不支持非对齐访存的宿主上生成不安全的 TCG 代码。

第二处错误来自 Ya2yOS 的 `MAX_MMAP_SIZE=2 GiB`。内层 QEMU 按 EFI 启动参数申请一个
2 GiB guest-RAM 匿名映射，另外还需要动态加载器和运行库的 VMA；总虚拟保留超过该进程配额
后，`mmap()` 正确地返回 `ENOMEM`，但默认 2 GiB 配额不再能支持这个目标工作负载。

### 修复

- `os/src/mm/memory_set/elf_loader.rs`：LoongArch64 上读取 `CPUCFG1.UAL`，仅在硬件确实
  支持时通过 `AT_HWCAP` 声明 Linux ABI 的 `HWCAP_LOONGARCH_UAL`；RISC-V 保持既有零值。
- `os/src/arch/loongarch64/qemu/memory_layout.rs`：将懒 `mmap` 的单进程虚拟配额从 2 GiB
  提升为 4 GiB。映射仍按页延迟分配，保留有限上界。

### 验证

- `make TARGET_ARCH=loongarch64` 通过；该顶层默认目标实际完成 RISC-V 与 LoongArch64
  release 构建。
- 修复前运行确认首个 UAL 错误消失，随后稳定复现 guest RAM `ENOMEM`。
- 修复后以 `timeout 120s make run TARGET_ARCH=loongarch64` 重放，日志包含内层 EFI/OVMF
  `PROGRESS CODE`，且不再出现 UAL 错误或 `cannot set up guest memory`。

该 120 秒窗口尚未等到内层 guest 的 `Hello, world!` 或正常关机，故完整嵌套 guest 回归仍待
更长的独立运行确认。
