# QEMU 10 高地址 FDT 启动映射

## 背景

RISC-V QEMU `virt` 平台通过 SBI 启动约定在 `a1` 传递 FDT 的物理地址。Ya2yOS 的
`entry.asm` 在进入 Rust 代码前启用 Sv39，并由 `arch::hardware::init_from_fdt()` 立即读取
该地址，以获得 RAM、Hart 数和 timebase。

## 现象

Docker 镜像 `zhouzhouyi/os-contest:20260510` 内的 QEMU 10.0.2 以默认
`make run` 参数启动 16 GiB、8 hart RISC-V guest 时，`log.ans` 在 OpenSBI 信息后停止，
没有任何 `[kernel]` 输出，QEMU 持续占用一个 CPU 核。

OpenSBI 报告 `Next Address = 0x80200000`、`Next Arg1 = 0x47fe00000`。相同内核和参数在宿主
QEMU 9.2.1 中的 `Next Arg1` 为 `0xbfe00000`，并能完成内核启动；同一 Docker QEMU 10.0.2
把内存降到 1 GiB 后也传递 `0xbfe00000` 并正常启动。

## 分析

`entry.asm` 原本仅以 1 GiB leaf PTE 恒等映射 `0x80000000..0xc0000000`，同时建立内核高半区
别名。QEMU 10 在 16 GiB guest RAM 下把 FDT 放在 RAM 末端附近的 `0x47fe00000`，不在这个
启动期恒等映射范围内。

汇编已经启用 `satp` 后才调用 `trampoline`，后者将 FDT 参数原样传入 `rust_main()`。
`init_from_fdt()` 直接读取 FDT magic；高地址地址未映射会在 `trap::init()` 前触发异常，因而
没有可用的 trap 日志，表现为 OpenSBI 后的忙循环。

## 根因

启动页表把 QEMU 9 的低地址 FDT 放置行为误当成固定契约。RAM 容量增大或 QEMU 版本改变后，
启动器可以合法地把 FDT 放到不同的 RAM 段，而内核没有在分页切换前映射该段。

## 修复

`os/src/arch/riscv64/qemu/asms/entry.asm` 现在在 `satp` 生效前：

- 从 `a1` 计算 Sv39 根页表索引 `a1 >> 30`；
- 仅在索引落入 512 个根项时，写入 FDT 所在 1 GiB 物理段的 `VRWXAD` 恒等 leaf PTE；
- 保留原有首 1 GiB 内核映射、高半区别名及 CMA 首 1 GiB 的早期分配策略。

这样无需把 QEMU 的 `MEMORY_SIZE` 复制为新的汇编常量，FDT 可随启动器在受 Sv39 根页表支持的
物理范围内移动。

## 涉及文件

| 文件 | 修改 |
| --- | --- |
| `os/src/arch/riscv64/qemu/asms/entry.asm` | 分页开启前动态映射 FDT 所在 1 GiB physical leaf |
| `os/src/mm/frame_alloc/buddy_cma.rs` | 更正早期 CMA 映射边界的注释 |
| `Docs/决赛文档/*` | 开发日志、问题索引及 AI 协作记录 |

## 验证

- Docker QEMU 10.0.2、`-m 16G`、8 hart 的修复前日志：FDT 为 `0x47fe00000`，无内核输出。
- Docker 内 `make build-arch TARGET_ARCH=riscv64`：生成更新后的 `kernel-rv`。
- `timeout 12 make run`（Docker QEMU 10.0.2，原始 16 GiB 参数）：仍收到
  `Next Arg1 = 0x47fe00000`，随后输出硬件探测、内存初始化、八核启动；全部 cagent 项完成并输出
  `BUILDSTORM_TOOLCHAIN ok`。时限结束时主动终止，因此未将 BuildStorm 全程作为已通过项目。
- `git diff --check`：通过。
