# ELF 部分文件末页泄漏到 BSS

## 背景

主程序的页对齐 `PT_LOAD` 段使用 private file-backed lazy VMA，以避免每次
`execve()` 都预先复制未访问的代码与只读数据。动态解释器继续走 eager framed
映射，因为它在启动期需要修改自身重定位状态。

ELF 规范要求 `p_memsz > p_filesz` 的尾部为零初始化。这个规则不仅适用于额外的
整页，也适用于包含 `p_filesz` 末尾的同一页：该页内的其余字节不能暴露可执行文件
在该段之后的内容。

## 现象

LoongArch64 final 镜像执行 CAgent 时稳定出现：

```text
PagePrivilegeIllegal in application, non-canonical bad addr = 0x617461646f723e,
bad instruction = 0xb554, sending SIGSEGV
```

`ESTAT=0x480800` 表明这是 `ADEM`（MemoryAccessAddressError），而不是普通的
PTE 权限错误。故障进程为 `/usr/bin/find`，`sepc=0xb554` 对应：

```text
ldptr.w $r14, $r12, 16
```

现场的 `$r12=0x617461646f722e` 是小端字符串 `".rodata"`，因此该指令把一个
ELF section-name 字符串误当作结构体指针解引用。

## 分析

从 final LoongArch64 镜像提取的 `/usr/bin/find` 有一个可写 `PT_LOAD`：

```text
vaddr   = 0x3a000
filesz  = 0x2508
memsz   = 0x2f38
```

有效文件数据应在 `0x3c508` 结束，而该页直到 `0x3d000` 才结束。旧的
`map_elf_lazy_file()` 按 `ceil(start + p_filesz)` 建立 file-backed VMA，因此把
`[0x3c000, 0x3d000)` 整页映射为文件内容。文件偏移 `0x3c610` 恰好是 section
header string table 的 `".rodata"`，覆盖了本应位于 `.bss` 的零值指针。

动态链接器完成重定位后，`find` 在 `0xb54c` 从全局对象读取该字段，随后在
`0xb554` 解引用，形成非规范地址 `0x617461646f723e`。因此异常标签中的
`PagePrivilegeIllegal` 只是当前 LoongArch trap 解码对 ADEM 的泛化名称，并非根因。

排查同时确认当前 hart 使用的页表 token 与进程 `MemorySet` token 一致，单 hart 和
12 hart 均可复现；这排除了远程 TLB、错误地址空间和陈旧 `BADV` 的方向。

## 根因

页对齐主 ELF 的 lazy loader 没有将最后一个不完整文件页拆开处理。它把包含
`p_filesz` 末尾的整页直接映射为文件页，违反了 ELF 的零填充语义，使文件尾的
section header / section-name 数据泄漏到 `.bss`。

## 修复

`os/src/mm/memory_set/elf_loader.rs` 现在按以下规则加载页对齐主程序段：

- `p_filesz` 覆盖的完整页继续作为 `MAP_PRIVATE` file-backed lazy VMA；
- 含 `p_filesz` 末尾的部分页改为 framed `MapAreaType::Elf`，只从文件复制有效的
  部分字节，剩余字节保持为零；
- 该部分页之后的完整 BSS 页继续使用匿名 lazy VMA，保留原有的按需分配边界；
- `p_filesz` 恰好页对齐时，保留原有的匿名 lazy BSS 路径。

本轮还修正两个 LoongArch trap 上下文完整性问题：

- `__alltraps` 以完整用户 `TrapContext` 为栈，必须保存 `origin_a0` 和 `PRMD` 到
  98/99 槽；仅 `__kern_trap` 的 256-byte 内核帧不能访问这两个槽。
- 返回用户态时，在将 trap entry 切换到 `__alltraps` 前关闭 `CRMD.IE`；`ertn` 再从
  已保存的 `PRMD` 恢复用户中断状态，避免内核中断被按用户 trap 解析并覆写 context。

后二者是并存的 trap 正确性修复，不是本次 `/usr/bin/find` ADEM 的直接根因。

## 涉及文件

- `os/src/mm/memory_set/elf_loader.rs`
- `os/src/arch/loongarch64/qemu/asms/trap.S`
- `os/src/trap/mod.rs`
- `Docs/决赛文档/problem/elf-partial-page-bss-leak.md`

## 验证

- `make build-arch TARGET_ARCH=loongarch64 KERNEL_EXTRA_FEATURES=fault-diagnostics`：通过。
- 使用 final `sdcard-la.img` 的独立 qcow2 覆盖层运行 12-hart LoongArch64 CAgent：
  修复前稳定记录 `/usr/bin/find` 的 ADEM；修复后日志不含 `PagePrivilegeIllegal`、
  `user_fault_signal`、panic、`TFAIL` 或 `TBROK`。十项 CAgent 均为 `pass`，并输出
  `BUILDSTORM_TOOLCHAIN ok` 与 `BUILDSTORM_MINIBUILD ok`。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- `make build-arch TARGET_ARCH=riscv64`：通过。
- 完整 BuildStorm 和 LTP 尚未运行；QEMU 在确认 CAgent 与 BuildStorm 冒烟标记后由
  宿主侧终止，因为测试入口不会自动关闭虚拟机。
