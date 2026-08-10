# LoongArch 固定 helper 地址与 `rt_sigreturn` 入口混用

## 背景

LoongArch 信号处理函数返回时不能直接恢复被中断的寄存器。内核投递信号时需要为 handler
准备一个返回入口；handler 执行 `ret` 后，该入口发起 `rt_sigreturn(139)`，内核再从用户栈上的
signal frame 恢复寄存器、信号掩码和备用信号栈状态。

Ya2yOS 没有实现完整的 Linux LoongArch vDSO ELF。它采用一页内核私有、用户只读可执行的
trampoline，并由 `setup_frame()` 主动把该入口写入 handler 的 `ra`。因此这个地址只需要满足：

- 内核构造 signal frame 时写入正确入口；
- 每个用户页表都将入口映射为 `R | X | U`；
- 该入口只在存在合法 signal frame 时执行 syscall 139；
- 它不能与需要正常返回的用户态 helper 共用同一入口。

## 第一阶段现象与误判

早期 `server.ans` 中，PID 1568、1569、1570 都发生了：

```text
cause = FetchInstructionPageFault
stval = sepc = 0xfffffffffffe4c44
pte_flags = None
area = None
```

三个不同地址空间在相同地址取指且都没有 PTE/VMA。第一阶段据此推断该地址是
BuildStorm/glibc 期望的 `__vdso_rt_sigreturn`，并做了以下修改：

- 将 `sigreturn_va()` 从 `0xffff_ffff_f000_0000` 改为
  `0xffff_ffff_fffe_4c44`；
- 将 `sigreturn_trampoline` 填充到物理页内 `+0xc44`；
- 把 `0xffff_ffff_fffe_4000` 映射到该 trampoline 页。

该修改解决了“页面未映射”这一表面现象，但没有证明调用者正在从 signal handler 返回。
它把“固定地址取指”直接等同于“signal restorer”，因而修错了入口语义。

## 新日志给出的决定性证据

增加成功 `setup_frame()` provenance、signal frame magic 扫描和用户寄存器诊断后，新的
`log.ans` 在 BuildStorm 正式编译阶段记录了 PID 1572、1573、1574：

```text
restore_frame: invalid frame magic ...
syscall_pc=0xfffffffffffe4c4c
user_ra=0x1500001084
has no successful setup_frame record
no 0xdeadbeef ...
PagePrivilegeIllegal ... sepc=0xfffffffffffe4c4c
```

这些信息排除了 signal handler 返回路径：

- 当前线程从创建以来没有成功构造过 signal frame；
- 当前栈附近没有 Ya2yOS signal frame 的 `0xdeadbeef` magic；
- `$ra=0x1500001084` 是普通函数调用的返回地址，而不是内核写入的 signal restorer；
- syscall 发生后 PC 前进到 `0xfffffffffffe4c4c`，说明执行的正是原 trampoline 中
  `0xfffffffffffe4c44: addi.d a7, 139` 和 `+0x4: syscall 0` 两条指令。

为确认 `$ra` 的含义，使用 `debugfs` 从 BuildStorm 实际的 LoongArch ext4 镜像副本中提取
glibc 2.36 动态链接器并反汇编。镜像内 `ld-linux-loongarch-lp64d.so.1` 的 Build ID 为
`53d6f35e29f776b51b880a6ae3acd41314c3c9bb`，对应代码为：

```asm
0000000000001078: ld.d  $r12, $r3, 8
0000000000001080: jirl  $r1, $r12, 0
0000000000001084: ...
```

该位置属于 `_dl_catch_exception`。运行时基址为 `0x1500000000`，所以日志中的
`0x1500001084` 正是间接调用返回点。也就是说，动态链接器把
`0xfffffffffffe4c44` 当作一个需要返回的普通 helper 调用，而不是执行 signal handler 的
`ret`。

## Linux 实际 ABI 与固定地址误区

放弃把 `0xffff_ffff_fffe_4c44` 当作固定 `rt_sigreturn`，并不是偏离 Linux ABI；相反，
硬编码这个绝对地址才不符合 Linux 当前的 LoongArch vDSO 机制。

Linux 7.0 的 LoongArch 实现流程是：

1. `arch_setup_additional_pages()` 把一份完整、有效的 vDSO ELF 映射到每个进程的地址空间；
2. `ARCH_DLINFO` 通过 `AT_SYSINFO_EHDR` 把该进程的 vDSO ELF 基址告诉动态链接器；
3. 内核保存每个 vDSO image 中 `__vdso_rt_sigreturn` 的符号偏移；
4. `setup_rt_frame()` 使用“当前进程 vDSO 基址 + `offset_sigreturn`”设置 handler 的返回地址。

对应 Linux 源码包括：

- `arch/loongarch/kernel/vdso.c`
- `arch/loongarch/include/asm/elf.h`
- `arch/loongarch/kernel/signal.c`
- `arch/loongarch/vdso/sigreturn.S`
- `arch/loongarch/vdso/vdso.lds.S`

因此 Linux 只固定了符号和调用语义，没有规定所有进程、所有内核版本、所有 vDSO 构建都必须
在绝对地址 `0xffff_ffff_fffe_4c44` 执行 `__vdso_rt_sigreturn`。页内 `0xc44` 也只是某个
具体 ELF 链接布局可能产生的符号偏移，不能脱离对应的 vDSO ELF、符号表和
`AT_SYSINFO_EHDR` 单独作为 ABI 常量使用。

当前 Ya2yOS 既没有映射完整 vDSO ELF，也没有在 auxv 中发布有效的 `AT_SYSINFO_EHDR`，更没有
根据实际 vDSO image 解析 `offset_sigreturn`。所以不能只截取 Linux 的某个运行时地址，把一页
私有裸 trampoline 伪装成完整 vDSO。

## 根因

根因是把两个调用约束相反的入口混为一谈：

| 入口 | 调用前提 | 返回行为 |
|------|----------|----------|
| signal `rt_sigreturn` restorer | `sp` 必须指向内核构造的合法 signal frame | 成功后恢复旧上下文，不返回原调用者 |
| BuildStorm glibc 固定 helper | 普通用户态间接调用，栈是动态链接器函数栈 | 必须通过 `$ra` 正常返回 |

错误 trampoline 在普通 helper 调用中执行 syscall 139。`restore_frame()` 从动态链接器普通栈帧
读取 magic，必然得到 `EINVAL`；系统调用返回后 PC 从 syscall 指令前进到
`0xfffffffffffe4c4c`，落入没有有效 helper 指令的区域并触发 `PagePrivilegeIllegal`。

这也解释了为什么仅把 `rt_sigreturn` 的非法 frame 从 panic 改为 `EINVAL` 不能解决问题：
`EINVAL` 只是避免内核 panic，错误的入口类型仍会让用户态继续在错误代码页执行。

## 修复

### 1. 恢复独立 signal restorer

`sigreturn_va()` 恢复为：

```text
0xffff_ffff_f000_0000
```

这是 Ya2yOS 的内核私有 signal ABI 地址。它不需要由 glibc 发现，因为只有内核
`setup_frame()` 会把它写入 handler 的 `ra`。对应物理页内 `+0x0` 保留：

```asm
sigreturn_trampoline:
    addi.d $a7, $zero, 139
    syscall 0
```

### 2. 保留而不是删除 `0xfffffffffffe4c44`

本次并未取消对该地址的兼容。用户页表仍映射
`0xffff_ffff_fffe_4000`，但页内 `+0xc44` 改为一个正常返回的兼容入口：

```asm
legacy_vdso_fallback:
    addi.w $a0, $zero, -38   # -ENOSYS
    jirl   $zero, $ra, 0
```

`-ENOSYS` 明确表示 Ya2yOS 没有实现该 vDSO/helper 快速路径，调用方可以按“不支持”路径处理或
回退到普通 syscall。关键点是该入口会返回到 `$ra`，不会再把普通动态链接器栈解释成 signal
frame。

### 3. 同一物理页、两个虚拟入口

两个用户虚拟页都以 `R | X | U` 映射到同一物理 trampoline 页：

```text
0xffff_ffff_f000_0000 + 0x000 -> sigreturn_trampoline
0xffff_ffff_fffe_4000 + 0xc44 -> legacy_vdso_fallback
```

这样既不额外分配物理页，也不会复用错误的入口语义。

## 为什么选择 `0xffff_ffff_f000_0000`

恢复该地址的原因不是它属于 Linux 公共 ABI，而是它适合作为 Ya2yOS 当前实现阶段的内核私有
restorer：

- 地址由内核单方面写入 signal handler 的 `ra`，不依赖用户态硬编码；
- 每个用户页表都建立同样的只读可执行映射；
- 与 BuildStorm glibc 已使用的 `0xffff_ffff_fffe_4000` 页分离；
- 页内 `+0x0` 与物理符号偏移一致，链接和页表关系简单；
- 未来实现完整 Linux vDSO 时，可以整体替换为
  `AT_SYSINFO_EHDR + offset_sigreturn`，不需要继续维护 `+0xc44` 这一错误假设。

它仍然是私有实现约定，不应作为 Ya2yOS 对用户程序公开的永久绝对 ABI。

## 后续完整 vDSO 方向

若后续需要实现真正的 Linux LoongArch vDSO，应一次性完成：

- 构建并映射合法的 vDSO ELF image；
- 在 auxv 中写入有效的 `AT_SYSINFO_EHDR`；
- 导出 `__vdso_rt_sigreturn` 和需要支持的时间/CPU helper 符号；
- 由实际 ELF 符号偏移生成 `offset_sigreturn`；
- signal 投递使用每进程 vDSO 基址加偏移。

在这些条件满足前，不应再次把日志中出现的某个绝对地址直接认定为 Linux 固定 ABI。

## 涉及文件

- `os/src/arch/loongarch64/qemu/memory_layout.rs`
- `os/src/arch/loongarch64/qemu/page_table.rs`
- `os/src/arch/loongarch64/qemu/asms/trap.S`
- `os/src/signal/types.rs`
- `os/src/signal/frame.rs`
- `Docs/决赛文档/problem/loongarch-signal-vdso-sigreturn.md`

`NormalSignalFrame`、`SigInfoSignalFrame` 是 signal frame ABI 的完整布局类型，不能在问题定位后
删除。`SignalFrameTrace` 只在 `fault-diagnostics` 构建中用于 provenance 诊断，可以在维护者完成
长程回归并确认问题闭环后单独删除。

## 验证

本次修复执行了编译和静态检查，没有按维护者要求运行 QEMU、LTP、CAgent 或完整 BuildStorm：

- `make build-arch TARGET_ARCH=loongarch64`：通过；
- `make build-arch TARGET_ARCH=loongarch64 KERNEL_EXTRA_FEATURES=fault-diagnostics`：通过；
- `make build-arch TARGET_ARCH=riscv64`：通过；
- `git diff --check`：通过。

链接后符号位置为：

```text
9000000000202000 T sigreturn_trampoline
9000000000202c44 T legacy_vdso_fallback
```

反汇编确认 signal 入口执行 syscall 139，兼容入口位于同一物理页 `+0xc44` 并返回
`-ENOSYS`。`rustfmt --check` 只报告 `memory_layout.rs` 中本次修改前已经存在的注释排版差异，
未为此引入无关格式化。

维护者后续运行 BuildStorm 时，应重点确认：

- PID 1572/1573/1574 一类普通调用不再出现 `invalid frame magic`；
- 不再在 `0xfffffffffffe4c4c` 发生 `PagePrivilegeIllegal`；
- 合法 signal handler 的 `selected_restorer` 为 `0xffff_ffff_f000_0000`；
- 合法 signal frame 仍能由 syscall 139 正常恢复。
