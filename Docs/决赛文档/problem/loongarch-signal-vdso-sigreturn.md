# LoongArch signal-vDSO `rt_sigreturn` 入口映射

## 背景

BuildStorm 会启动大量会安装和返回信号处理函数的 LoongArch64 子进程。处理函数执行完成后，
用户态必须跳转到 `rt_sigreturn` 入口，陷入内核恢复被中断的寄存器和信号掩码。

Ya2yOS 的 LoongArch 用户页表为此保留了一页只读、可执行的私有 trampoline；信号帧创建代码
把该入口写入用户 TrapContext 的 `ra`。因此入口虚拟地址、用户页表映射和 trampoline 的页内
偏移必须一致。

## 现象

`server.ans` 的原始物理行 `11389`--`11391` 连续记录三个进程的相同异常：

```text
PID 1568/1569/1570
cause = FetchInstructionPageFault
stval = sepc = 0xfffffffffffe4c44
pte_flags = None
area = None
```

`strings` 去除 ANSI 控制字符后，这三条记录位于 `11417`--`11419`；维护者提到的
第 `11421` 行是紧随其后的 perf 统计。无论采用哪种计数方式，故障地址均为
`0xffff_ffff_fffe_4c44`。

三个不同页表 token 的进程都在同一地址取指，且诊断同时显示 PTE、叶子 PPN 和 VMA 均不存在。
这排除了单个进程地址空间损坏和错误页权限，更符合遗漏公共 signal-vDSO 返回页映射的特征。

## 分析

Linux 7.0 的 LoongArch 信号路径不采用用户传入的 `SA_RESTORER` 入口。它在
`arch_setup_additional_pages()` 中创建 vDSO 映射，并通过 `ARCH_DLINFO` 把 vDSO 基址放入
`AT_SYSINFO_EHDR`。投递信号时，`handle_signal()` 调用：

```c
setup_rt_frame(vdso + current->thread.vdso->offset_sigreturn, ...)
```

将该地址作为 handler 的返回地址。vDSO 中导出的 `__vdso_rt_sigreturn` 只执行：

```asm
li.w a7, __NR_rt_sigreturn
syscall 0
```

参见本地 Linux 7.0 源码：

- `arch/loongarch/kernel/vdso.c:68-114`
- `arch/loongarch/include/asm/elf.h:342-350`
- `arch/loongarch/kernel/signal.c:935-1014`
- `arch/loongarch/vdso/sigreturn.S:19-24`
- `arch/loongarch/vdso/vdso.lds.S:60-78`

Ya2yOS 的 `PageTable::new_user()` 已有把 `sigreturn_pa()` 所在物理页以
`R | X | U` 映射到 `sigreturn_va()` 所在用户页的机制。旧地址为
`0xffff_ffff_f000_0000`，不能覆盖日志中运行时需要的
`0xffff_ffff_fffe_4c44`。因此用户 handler 返回时会在未映射页面取指。

## 根因

LoongArch signal trampoline 使用的 Ya2yOS 私有虚拟地址与 BuildStorm/glibc 运行时使用的
LoongArch signal-vDSO `rt_sigreturn` 入口不一致。旧映射既没有覆盖
`0xffff_ffff_fffe_4000` 这一页，也没有让 trampoline 的代码落在该页内 `0xc44` 偏移，
最终在 handler 返回路径产生 `FetchInstructionPageFault`。

## 修复

- `os/src/arch/loongarch64/qemu/memory_layout.rs`：将 `sigreturn_va()` 改为
  `0xffff_ffff_fffe_4c44`。
- `os/src/arch/loongarch64/qemu/asms/trap.S`：在页对齐的 trampoline 段填充 `0xc44` 字节，
  使 `sigreturn_trampoline` 位于其物理页内相同偏移。

现有 `PageTable::new_user()` 无需修改。它仍以 `sigreturn_va().floor()` 和
`sigreturn_pa().floor()` 建立 `R | X | U` 映射；修复后自然映射
`0xffff_ffff_fffe_4000`，入口与物理符号的页内偏移都为 `0xc44`。

这是架构 ABI 兼容修复，不修改用户程序、QEMU 参数或 BuildStorm 脚本。

## 涉及文件

- `os/src/arch/loongarch64/qemu/memory_layout.rs`
- `os/src/arch/loongarch64/qemu/asms/trap.S`
- `Docs/决赛文档/problem/loongarch-signal-vdso-sigreturn.md`

## 验证

- `readelf -sW os/target/loongarch64-unknown-none/release/os | rg sigreturn_trampoline`：
  符号为 `0x9000000000202c44`。现有用户映射页基址为
  `0xffff_ffff_fffe_4000`，故用户入口为预期的 `0xffff_ffff_fffe_4c44`。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- `make build-arch TARGET_ARCH=riscv64`：通过。
- `timeout 180s make run TARGET_ARCH=loongarch64`：`sigaltstack`、`rseq`、`uptime`
  回归均为 `PASS`；十项 CAgent 全部通过，且输出 `BUILDSTORM_TOOLCHAIN ok` 和
  `BUILDSTORM_MINIBUILD ok`。日志未包含 `user_fault_signal`、
  `FetchInstructionPageFault`、panic、`TFAIL` 或 `TBROK`。

原始 `server.ans` 的异常出现在约 `3698s` 的长程 BuildStorm 过程中。本轮 180 秒运行停在
`pre-build tg-xtask`，尚未覆盖该完整窗口，因此不能据此声称已经完成端到端 BuildStorm/LTP
回归。
