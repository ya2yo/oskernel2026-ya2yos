# Loongson 2K1000 hugepage 内存布局常量遗漏导致构建失败

## 背景

Ya2yOS 的用户态匿名 `MAP_HUGETLB` 最小实现使用 LoongArch level-1 的 2 MiB 页表叶子。
`mmap`、VMA 管理和页表映射通过 architecture memory layout 获取 hugepage 的字节大小和
对应 4 KiB 基页数。

## 现象

执行：

```bash
make build-arch TARGET_ARCH=loongarch64 PLATFORM=2k1000
```

用户态程序构建完成后，内核编译报错：

```text
error[E0432]: unresolved import `crate::arch::memory_layout::HUGE_PAGE_PAGES`
error[E0432]: unresolved import `crate::arch::memory_layout::HUGE_PAGE_SIZE`
error[E0425]: cannot find value `HUGE_PAGE_SIZE` in module `crate::arch::memory_layout`
```

## 分析

`PLATFORM=2k1000` 令 `crate::arch::memory_layout` 解析为
`os/src/arch/loongarch64/board_2k1000/memory_layout.rs`。通用的
`map_area.rs`、`memory_set/mmap_ops.rs` 和 `syscall/mm/mmap.rs` 均使用 hugepage 常量。

匿名 hugepage 功能在 `aaa640de` 中为 RISC-V QEMU 和 LoongArch QEMU 布局加入了
`HUGE_PAGE_SIZE = 0x20_0000`、`HUGE_PAGE_PAGES = HUGE_PAGE_SIZE / PAGE_SIZE`，但没有
同步更新 2K1000 布局。2K1000 仍采用 LoongArch 4 KiB 基页及 level-1 2 MiB leaf，因此
常量语义与 QEMU 平台一致。

## 根因

新增的架构内存布局接口没有覆盖已有的 LoongArch 2K1000 平台实现，导致只在选择该平台时
发生名称解析失败。

## 修复

将 LoongArch 的共同页几何集中到
`os/src/arch/loongarch64/page_geometry.rs`：

```rust
pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = 12;
pub const HUGE_PAGE_SIZE: usize = 0x20_0000;
pub const HUGE_PAGE_PAGES: usize = HUGE_PAGE_SIZE / PAGE_SIZE;
```

QEMU 与 2K1000 的 `memory_layout.rs` 都公开重导出这些常量，因而原有调用路径保持为
`crate::arch::memory_layout::{PAGE_SIZE, PAGE_SIZE_BITS, HUGE_PAGE_SIZE, HUGE_PAGE_PAGES}`。
板级文件仍只保存 UART、DRAM、MMIO、链接地址和容量限制等平台差异，不修改 mmap 语义、
页表实现、测试入口或构建参数。

为保持两种架构的边界对称，RISC-V 同样新增
`os/src/arch/riscv64/page_geometry.rs`。QEMU `virt` 与 VisionFive 2 继续共用
`riscv64/qemu/memory_layout.rs`，该布局重导出 RISC-V 的页几何常量，并保留
`visionfive2` feature 对启动、MMIO、地址与驱动的选择。两个架构分别维护自己的页几何，
不将当前恰好相同的 4 KiB/2 MiB 数值上提为跨架构常量。

## 涉及文件

- `os/src/arch/loongarch64/mod.rs`
- `os/src/arch/loongarch64/page_geometry.rs`
- `os/src/arch/loongarch64/qemu/memory_layout.rs`
- `os/src/arch/loongarch64/board_2k1000/memory_layout.rs`
- `os/src/arch/riscv64/mod.rs`
- `os/src/arch/riscv64/page_geometry.rs`
- `os/src/arch/riscv64/qemu/memory_layout.rs`
- `Docs/决赛文档/problem/loongarch-2k1000-hugepage-layout-build.md`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

- `make build-arch TARGET_ARCH=loongarch64 PLATFORM=2k1000`：通过，生成 2K1000 raw image。
- `make build-arch TARGET_ARCH=loongarch64 PLATFORM=qemu`：通过。
- `make build-arch TARGET_ARCH=riscv64 PLATFORM=qemu`：通过。
- `make build-arch TARGET_ARCH=riscv64 PLATFORM=visionfive2`：通过，生成 `kernel-vf2`。

构建输出仅包含现有 `smoltcp` unused warning 和 Cargo `config` 弃用提示。未执行
2K1000 实板或 QEMU 运行时 hugepage 测例；本次问题是编译期接口遗漏，运行时语义未改动。
