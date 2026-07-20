# RISC-V 双 hart BuildStorm 启动栈越界与 .data 破坏

## 背景

RISC-V QEMU 以双 hart 启动。调度器和每 task 的 kernel stack 建立前，bootstrap hart 已会进入
文件系统、网络和 VirtIO 初始化，因此仍依赖 `entry.asm` 中按 hart 分配的早期启动栈。

## 现象

在 OpenSBI 选择 HART1 为 boot hart 时，定向日志在 `net::init` 前后出现：

```text
Cause: Exception(LoadPageFault)
stval: 0x72616883
sepc : 0xffffffc08032984c
```

`sepc` 对应 `log::GlobalLogger::log` 的 trait-vtable 读取，`stval` 是损坏的 vtable 地址，
不是有效内核虚拟地址。

## 分析与根因

原每 hart bootstrap stack 只有 64 KiB。HART1 的栈区间为
`0xffffffc08039d000..0xffffffc0803ad000`，相邻下方 `.data` 中的 `log::LOGGER` 位于
`0xffffffc08039c728/730`。

release ELF 反汇编显示启动调用链至少有以下固定栈帧：

- `rust_main`：`0x7c20`，31,776 B；
- `NetDeviceImpl::try_new_device`：`0x7410`，29,712 B；
- `VirtIONetRaw::new`：`0x4280`，17,024 B。

合计 `0x132b0`，即 78,512 B，已超过 64 KiB，且尚未包括更深层调用。向下增长的栈会写入
相邻 `.data`，破坏 `log::LOGGER` 的 vtable 指针。HART0 启动时溢出首先进入 HART1 的预留栈，
过去的“正常”启动只是内存布局掩盖，不能作为有效回归。

## 修复

在 `os/src/arch/riscv64/qemu/asms/entry.asm` 定义：

```asm
.equ BOOT_STACK_SHIFT, 17
.equ BOOT_STACK_SIZE, (1 << BOOT_STACK_SHIFT)
.equ BOOT_HARTS, 2
```

每 hart 栈扩大至 128 KiB，同时用 `BOOT_STACK_SHIFT` 计算 hart stride，用
`BOOT_STACK_SIZE * BOOT_HARTS` 分配区域。stride 与预留空间必须同步，否则两个 hart 的栈会
重叠。`BOOT_HARTS` 需要与 `os/src/config.rs::HART_NUM` 保持一致。

## 验证

修复后 RISC-V 定向启动已越过原先 `net::init` panic 点，并继续出现
`create_init_files success!` 和 `sigaltstack regression: PASS`；未再观察到原
`LoadPageFault` 或 logger vtable 破坏。最终整理后 RISC-V 和 LoongArch64 release 构建均通过。

本轮未完成完整 BuildStorm QEMU 运行，因此该结论是启动路径与编译级验证，不宣称决赛测例
整体完成。
