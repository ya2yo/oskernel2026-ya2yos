# LoongArch 跨 Hart 指令流失同步误导为 signal/vDSO 故障

## 背景

LoongArch 用户态 signal handler 执行 `ret` 时，需要跳到一个能够发起
`rt_sigreturn(139)` 的 restorer。Ya2yOS 当前没有实现完整的 Linux LoongArch vDSO ELF，
而是映射一页内核私有、用户只读可执行的 trampoline，并由 `setup_frame()` 主动把
`sigreturn_va()` 写入 handler 的 `ra`：

```asm
sigreturn_trampoline:
    addi.d $a7, $zero, 139
    syscall 0
```

当前私有 restorer 的虚拟地址是 `0xffff_ffff_f000_0000`。该地址只由内核构造 signal
frame 时使用，不要求 libc 发现，也不属于对用户态公开的永久 Linux ABI。

本问题先后出现了三组看似指向 signal/vDSO、实际由用户指令流失同步引起的症状。前两次
修复只改变了故障表现，第三组日志才给出了能区分“错误函数指针”和“运行时指令与 ELF
不一致”的决定性证据。

## 第一阶段：误判为固定 `rt_sigreturn`

早期 `server.ans` 中，PID 1568、1569、1570 都发生：

```text
cause = FetchInstructionPageFault
stval = sepc = 0xfffffffffffe4c44
pte_flags = None
area = None
```

当时把三个进程共同访问的地址直接解释为 Linux 固定 `__vdso_rt_sigreturn`，于是把
Ya2yOS trampoline 移到物理页内 `+0xc44`，并把
`0xffff_ffff_fffe_4000` 映射为用户可执行页。

这个改动只证明“映射该页可以让取指继续”，没有证明调用来自 signal handler。短程回归
没有覆盖 BuildStorm 数千秒后才出现的异常，因此也不能证明地址语义正确。

## 第二阶段：误判为普通 helper

加入 signal frame provenance 和栈诊断后，PID 1572--1574 显示：

```text
restore_frame: invalid frame magic ...
syscall_pc=0xfffffffffffe4c4c
user_ra=0x1500001084
no successful setup_frame record
no 0xdeadbeef signal-frame magic
```

实际动态链接器反汇编为：

```asm
0000000000001078: ld.d  $r12, $r3, 8
0000000000001080: jirl  $r1, $r12, 0
0000000000001084: ...
```

`$ra=0x1500001084` 证明动态链接器通过 `$r12` 做了一次普通间接调用；它只说明返回点位于
`_dl_catch_exception`，并不能证明 `$r12=0xfffffffffffe4c44` 本身是有效 ABI helper。
此前把这两件事混为一谈，在该地址放置了“返回 `-ENOSYS`”的
`legacy_vdso_fallback`。这个入口避免了错误执行 syscall 139，但仍然把已经损坏的控制流
伪装成可继续执行的正常路径。

## 第三阶段：合法 ELF 指令在运行时异常

后续日志出现了与固定高地址无关的新故障：

```text
PID 1568: PagePrivilegeIllegal
stval=0x170de53000000000 sepc=0x2a17304418

PID 1569: IllegalInstruction at 0x2a173021c4
PID 1570: IllegalInstruction at 0x2a173021c4
```

从实际 BuildStorm 镜像提取 `libc.so.6`，其运行时基址为 `0x2a17200000`。两个 PC 对应
libc 文件偏移及指令如下：

| 运行时 PC | ELF 偏移 | ELF 中的指令 |
|---|---:|---|
| `0x2a173021c4` | `0x1021c4` | `57fcafff  bl 0x101e70` |
| `0x2a17304418` | `0x104418` | `558d3bfc  bl 0x1d150` |

两处都是合法的 LoongArch `bl`。同一地址在实际 ELF 中是合法指令，运行时却分别被报告为
`IllegalInstruction`，或表现得像访问了非 canonical 地址，说明发生异常时 Hart 取到的
指令流与当前页表所指向的 ELF 内容不一致。此前的非法 `rt_sigreturn`、损坏回调地址和
`0xfffffffffffe4c44` 是同一类指令流错误继续执行后的次生症状，而不是根因。

## 根因

Ya2yOS 已在以下路径执行 `instruction_fence()`：

- 当前 Hart 装入新的文件可执行页或匿名可执行页；
- COW、`mprotect`、`munmap` 等页表更新触发本地或远端 shootdown；
- present executable fault 的本地重试。

遗漏的边界是 `MemorySet::activate_for_user()`。任务被调度到另一个 Hart 时，原实现只激活
页表并发布 `active_harts` 位，没有在这个 Hart 上执行 LoongArch `ibar 0`。可执行物理页可能
在该 Hart 之前运行其他地址空间时留下旧指令流，也可能由另一个 Hart 完成装入。仅切换页表
和失效 TLB 不足以建立本 Hart 的取指同步边界。

因果链为：

```text
可执行物理页被复用，或地址空间迁移到另一 Hart
    -> 新 Hart 激活用户页表，但没有执行 ibar
    -> 本地取指仍观察到旧的或错误的指令流
    -> 合法 libc bl 被当作非法指令，或执行成另一条访存/控制流指令
    -> 回调寄存器、返回地址或栈状态继续损坏
    -> 出现 0xfffffffffffe4c44、invalid frame magic 等次生症状
```

## 修复

### 1. 在地址空间重新进入 Hart 时同步指令流

`MemorySet::activate_current_hart()` 现在返回当前 Hart 的 active bit 是否由 0 变为 1。
`activate_for_user()` 在首次发布时执行架构 `instruction_fence()`；LoongArch 后端对应
`ibar 0`，RISC-V 后端对应 `fence.i`。

active bit 在任务真正离开处理器、`exec` 切换地址空间或页表写操作期间清除，不会在普通
syscall 入口清除。因此该 fence 位于“地址空间重新进入当前 Hart”的边界，不是每次 syscall
返回都执行。缺页、COW 和 shootdown 路径保留原有的即时 fence。

### 2. 删除 `0xfffffffffffe4c44` 假兼容入口

删除以下实现：

- `legacy_vdso_fallback`；
- `legacy_vdso_fallback_va()`；
- 用户页表对 `0xffff_ffff_fffe_4000` 的额外映射。

不能继续保留该入口，原因是：

- 实际 loader 只证明 `$ra` 是普通间接调用返回点，没有证明被调用地址是合法 helper；
- 实际 loader 和 libc ELF 中没有证据表明该绝对地址是固定函数入口；
- 返回 `-ENOSYS` 会掩盖损坏的函数指针，让错误控制流继续运行并在更远处失败；
- 该映射把非 ABI 地址变成可执行用户入口，扩大了内核与用户态之间不必要的固定约定。

### 3. 保留独立 signal restorer

`sigreturn_va()` 保持为 `0xffff_ffff_f000_0000`，页内 `+0x0` 只执行 syscall 139。
只有 `setup_frame()` 会把它写入 handler 的 `ra`，所以合法 signal 返回路径不依赖
`0xfffffffffffe4c44`。

## 为什么 Linux 也不能证明 `0xfffffffffffe4c44` 合法

Linux LoongArch 的真实流程是：

1. `arch_setup_additional_pages()` 映射完整的 vDSO ELF；
2. `ARCH_DLINFO` 通过 `AT_SYSINFO_EHDR` 发布该进程的 vDSO 基址；
3. 内核保存 vDSO image 中 `__vdso_rt_sigreturn` 的符号偏移；
4. `setup_rt_frame()` 使用“当前进程 vDSO 基址 + `offset_sigreturn`”设置 handler 返回地址。

Linux 固定的是符号语义，不是所有进程共享的绝对地址。页内 `0xc44` 即使曾出现在某个
vDSO image 中，也只是该 ELF 的链接布局结果，不能脱离 ELF、符号表、映射基址和
`AT_SYSINFO_EHDR` 单独成为 ABI 常量。

Ya2yOS 当前没有完整 LoongArch vDSO ELF，也没有发布与该 image 对应的有效
`AT_SYSINFO_EHDR`。因此私有 signal restorer 可以作为过渡实现，但不能截取 Linux 某次
运行中的绝对地址并伪装成完整 vDSO，更不能用该地址容忍已经损坏的用户控制流。

对应 Linux 7.0 源码位置包括：

- `arch/loongarch/kernel/vdso.c`
- `arch/loongarch/include/asm/elf.h`
- `arch/loongarch/kernel/signal.c`
- `arch/loongarch/vdso/sigreturn.S`
- `arch/loongarch/vdso/vdso.lds.S`

## Signal frame 类型能否删除

`NormalSignalFrame` 和 `SigInfoSignalFrame` 定义 signal ABI 的完整用户栈布局，供
`setup_frame()` 构造、`restore_frame()` 校验和恢复，不能在本问题修复后删除。它们放在
`os/src/signal/types.rs`，避免 frame 构造与恢复各自维护不一致的偏移。

`SignalFrameTrace` 只在 `fault-diagnostics` 特性下记录 provenance；待维护者完成长程行为
回归并确认不再需要该诊断后，可以作为独立清理删除，但不能连同 signal frame ABI 类型删除。

## 涉及文件

- `os/src/mm/memory_set/handle.rs`
- `os/src/arch/loongarch64/qemu/memory_layout.rs`
- `os/src/arch/loongarch64/qemu/page_table.rs`
- `os/src/arch/loongarch64/qemu/asms/trap.S`
- `os/src/signal/types.rs`
- `os/src/signal/frame.rs`
- `Docs/决赛文档/problem/loongarch-signal-vdso-sigreturn.md`

## 验证边界

本轮完成以下静态验证：

- `make build-arch TARGET_ARCH=loongarch64`：通过；
- `make build-arch TARGET_ARCH=loongarch64 KERNEL_EXTRA_FEATURES=fault-diagnostics`：通过；
- `make build-arch TARGET_ARCH=riscv64`：通过；
- `git diff --check`：通过。

按维护者要求，本轮不运行 QEMU、LTP、CAgent 或 BuildStorm。以上证据能够定位缺失的同步
边界并支持源码修复，但只有覆盖原异常窗口的长程行为回归才能确认故障已经闭环。回归时应
重点确认：

- 不再在 `0x2a173021c4` 报告 `IllegalInstruction`；
- 不再从 `0x2a17304418` 产生非 canonical 访问；
- 不再出现普通 loader 栈触发的 `invalid frame magic`；
- 合法 signal handler 仍从 `0xffff_ffff_f000_0000` 执行 syscall 139 并成功恢复。
