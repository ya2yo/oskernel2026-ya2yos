# LoongArch CAgent 动态链接器 LSX 未启用导致 panic

## 背景

final-2026 的 CAgent 通过 `/bin/bash cagent_testcode.sh` 启动。此前已修复 Bash
数组和 Debian `/bin -> /usr/bin` 中间符号链接解析，使该运行链路可以进入动态 ELF
解释器。本问题只发生在 LoongArch64 的动态链接器启动阶段，与那两个 VFS/脚本问题不同。

## 现象

`log.ans` 显示子进程已经成功完成 `execve("/bin/bash", ...)`、打开 Bash 和
`/lib64/ld-linux-loongarch-lp64d.so.1`，随后立即报错：

```text
[syscall ret --- OK] Execve ret = 0
Fail to convert LoongArch Unknown to Trap type! 0x10
panic
[kernel] Panicked at src/trap/mod.rs:230 Unsupported trap Unknown, stval = 0x15000177d4!
```

因此 CAgent 脚本尚未开始执行，不能把该 panic 归因于 agent_lite、HTTP 服务或某个
业务任务。

## 分析

`DL_INTERP_OFFSET` 为 `0x1500000000`，所以日志中的地址对应解释器内偏移
`0x177d4`。从 final-2026 LoongArch 镜像只读提取
`/usr/lib/loongarch64-linux-gnu/ld-linux-loongarch-lp64d.so.1` 后，反汇编结果为：

```text
177d4:  2c004182  vld $vr2,$r12,16(0x10)
177d8:  2c008181  vld $vr1,$r12,32(0x20)
177dc:  2c00c180  vld $vr0,$r12,48(0x30)
```

本地 Linux 7.0 的 `arch/loongarch/include/asm/loongarch.h` 定义
`EXCCODE_LSXDIS = 16`，即 ESTAT ecode `0x10` 是 LSX Disabled (SXD)，不是页错误。

Ya2yOS 的每个 hart 都经 `init_csr_regs()` 初始化，但旧代码只执行
`euen::set_fpe(true)`，没有设置 LSX 的 `EUEN.SXE`。同时，所用 loongArch64 依赖的
`estat::cause()` 仅解码到 ecode `0x0f`，会把 `0x10` 交给 `Trap::Unknown`；通用 trap
分发对该值直接 panic。

## 根因

内核没有为 LoongArch 用户态启用 LSX，而 final-2026 glibc 动态链接器在 Bash 启动前就
无条件执行 LSX `vld/vst`。仅把 ecode `0x10` 映射为 `SIGILL` 虽可避免内核 panic，但会
杀死 Bash，不能修通 CAgent。

此外，旧的 `FloatRegs` 只保存 32 个 64-bit FPR，`trap.S` 也只使用 `fst.d/fld.d`。若只
打开 `EUEN.SXE`，定时抢占、信号或并发任务切换将丢失每个 LSX 寄存器的高 64 位。

## 修复

- `os/src/arch/loongarch64/qemu/cpu.rs`
  - 在每个 hart 的 CSR 初始化中，保持 `EUEN.FPE` 并启用 `EUEN.SXE`。
- `os/src/arch/loongarch64/qemu/context/regs.rs`
  - 将每个寄存器槽扩展为 128-bit `[[usize; 2]; 32]`，并以 16-byte 对齐保存 LSX 状态。
- `os/src/arch/loongarch64/qemu/asms/trap.S`
  - 用 32 组 `vst/vld` 保存和恢复 `$vr0..$vr31`。
  - 同步调整 FCSR、FCC、`origin_a0`、PRMD 和 kernel stack 在 `TrapContext` 中的偏移。

`MachineContext` 和 `UserContext` 均整体复制 `FloatRegs`，因此 signal frame、fork 和
`rt_sigreturn` 会随结构扩展保留完整 LSX 状态。

## 验证

执行：

```text
make loongarch64-build
git diff --check
```

LoongArch64 release 构建通过。

维护者提供的修复后根目录 `log.ans` 已显示：

```text
#### OS COMP TEST GROUP START cagent ####
...
#### OS COMP TEST GROUP END cagent ####
shutdown!
```

日志中不存在 `panic`、`Unsupported trap`、`Unknown to Trap`、`SXD` 或 `ASXD`。CAgent 的
10 个业务任务中 6 项为 `pass`（kernel、fs-create、fs-search、network、date、fs-usage），
4 项为 `reject`（cpu、factorial、fs-readwrite、fs-directory）；后者是 agent 业务判定结果，
不属于本次动态链接器启动 panic，未被表述为已修复。脚本会并发启动多个 agent_lite 任务，完整
到达 group end 也覆盖了运行期间频繁 trap/调度下的 LSX 状态保存路径。

## 边界

本次只启用并保存 128-bit LSX 状态，没有启用 `EUEN.ASXE`。镜像内确实含有 LASX
`xvld/xvst` 的候选代码，但当前 `AT_HWCAP` 为 0，修复后的运行没有触发 ecode `0x11`。若未来
出现 LASX Disabled，则必须实现完整 256-bit LASX 状态保存/恢复，不能只调用
`euen::set_asxe(true)`。
