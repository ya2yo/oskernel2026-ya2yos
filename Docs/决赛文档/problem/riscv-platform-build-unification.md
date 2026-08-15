# RISC-V 平台构建入口统一

## 背景

Ya2yOS 同时支持 RISC-V QEMU `virt` 与 StarFive VisionFive 2。此前前者使用
`TARGET_ARCH=riscv64`，后者使用 `TARGET_ARCH=visionfive2`，把板型误作为 CPU 架构。

## 现象

VisionFive 2 的构建变量散落在独立 `scripts/visionfive2.mk`，与
`scripts/riscv64.mk` 复制 RISC-V target、工具链和通用产物定义。根 Makefile 需要把同一
RISC-V 架构分成两个 `TARGET_ARCH` 分支，平台 feature 的透传也与 LoongArch64 的
`PLATFORM=2k1000` 模式不一致。

## 根因

构建入口最初只区分 QEMU 和实板，没有把 CPU 架构与板级平台建模为两个维度。随着 2K1000
已经使用 `TARGET_ARCH=loongarch64 PLATFORM=2k1000`，VisionFive 2 的旧接口成为不对称的
历史遗留。

## 修复

- `scripts/riscv64.mk` 以 `PLATFORM ?= qemu` 选择 RISC-V 平台。
- `PLATFORM=visionfive2` 使用 2 GiB 内存、4 核、`kernel-vf2` 和
  `KERNEL_PLATFORM_FEATURES=visionfive2`；`PLATFORM=qemu` 保持原有 QEMU 参数与
  `kernel-rv`。
- 根 Makefile 只接受 `riscv64`、`loongarch64` 两种真实 CPU 架构，删除旧
  `scripts/visionfive2.mk`。
- `run` 对 VisionFive 2 输出实板 U-Boot 启动提示，不再尝试 QEMU `virt`。

现行 VisionFive 2 构建命令为：

```bash
make build-arch TARGET_ARCH=riscv64 PLATFORM=visionfive2
```

## 涉及文件

- `Makefile`
- `scripts/riscv64.mk`
- `scripts/visionfive2.mk`（删除）
- `Docs/vf2.txt`

## 验证

- `make build-arch TARGET_ARCH=riscv64 PLATFORM=visionfive2`
- `make build-arch TARGET_ARCH=riscv64 PLATFORM=qemu`
- `make build-arch TARGET_ARCH=loongarch64 PLATFORM=qemu`
- `make build-arch TARGET_ARCH=loongarch64 PLATFORM=2k1000`

上述构建和 ELF 入口检查在本次改动后执行。没有 VisionFive 2 实板，未执行 U-Boot、串口、
网卡或 SD/MMC 运行验证。
