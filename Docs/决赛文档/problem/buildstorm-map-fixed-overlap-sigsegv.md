# BuildStorm LoongArch64 MAP_FIXED 重叠 VMA 导致 rustc SIGSEGV

## 背景

LoongArch64 BuildStorm 在 Cargo 并行预构建阶段出现用户态 `StorePageFault`，随后 rustc
收到 `SIGSEGV` 并退出。日志启用了 `fault-diagnostics`，可以同时看到缺页地址、VMA 权限和
页表状态。

## 现象

`server.ans` 中 fault 地址为 `0x2a1b05c000`，诊断显示：

```text
area=[0x2a1b044,0x2a1b060) MapType::Framed map_perm_bits=16 mmap_flags_bits=50 resident_frame=false
pte_flags=None
```

该区域只有 `U` 权限，即用户态 `PROT_NONE` 区域；fault 发生在 Cargo 编译 `2/446` 左右，
rustc 随后报告 `interrupted by SIGSEGV`。

## 分析

LoongArch64 jemalloc 先以 `mmap(PROT_NONE)` 建立固定匿名 arena，随后使用
`mmap(MAP_FIXED, PROT_READ|PROT_WRITE)` 提交其中一段。该提交范围超出了诊断所示旧
VMA `[0x2a1b044,0x2a1b060)` 的右端。

旧代码只有在某个 VMA 完全包含提交范围时才调用 `mprotect`；范围不完全包含时直接追加
新 VMA，形成重叠的 area。缺页处理按 `areas` 顺序查找，先命中旧的 `PROT_NONE` area，
因此不会尝试新 VMA 的可写权限，页表也不会安装物理页。

## 根因

`MAP_FIXED` 没有实现 Linux 的替换语义：目标范围内旧的动态 mmap VMA 未被移除或裁剪，
而是与新 VMA 并存。VMA 顺序使旧的 `PROT_NONE` 映射遮蔽了后续可写映射。

## 修复

在 `MemorySetInner::mmap` 的 `MAP_FIXED` 路径中，先调用现有内部 `munmap` 清理目标范围
内重叠的动态 mmap VMA（包括部分重叠时的裁剪和页表解除映射），再插入新的 lazy mmap VMA。
`MAP_FIXED_NOREPLACE` 仍在发现任何重叠时返回 `EEXIST`，不执行替换。

## 涉及文件

- `os/src/mm/memory_set/mmap_ops.rs`

## 验证

- `make TARGET_ARCH=riscv64`：RISC-V 与 LoongArch64 release 构建均通过。
- 修复前观测版 LoongArch64 QEMU 复现确认了旧 `PROT_NONE` VMA 遮蔽新 VMA；日志在
  `2/446` 触发相同 `StorePageFault`。
- 修复后 LoongArch64 诊断版 QEMU 在 180 秒内推进到 Cargo `41/446`，没有再次出现
  `fault-diagnostics`、rustc `SIGSEGV`、panic 或 compiler error；外层 timeout 终止，
  尚未完成完整 446 crate BuildStorm。
