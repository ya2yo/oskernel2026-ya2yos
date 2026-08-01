# RISC-V BuildStorm rustc SIGSEGV 缺页现场诊断

## 背景

RISC-V BuildStorm 的长日志先后将 `find-msvc-tools`、`unicode-ident` 报为
`rustc ... (signal: 11, SIGSEGV)`。崩溃 crate 会随并发时序变化，因而只是受害进程，
不能据此把问题归因于某个 Cargo 依赖，也不能用 `RUST_MIN_STACK` 掩盖。

前一轮文件页缓存满载处理已经改为容量旁路，并将连续 `in_use` 候选移出热扫描路径；
该路径的统计正常，但仍需要取得发生 SIGSEGV 时的实际页表和信号现场。

## 现象

启用首版诊断的 RISC-V 240 秒 BuildStorm 复现，在 Cargo `Building 0/446` 时两次捕获同一故障地址：

```text
cause=Exception(FetchInstructionPageFault)
stval=sepc=0x2a0b92ec08
vma=Mmap, map_type=Framed, map_perm_bits=26 (R|X|U)
file_backed=true, resident_frame=true, mapped_ppn=Some(...)
```

随后 Rust 的 SIGSEGV handler 被调用并打印 backtrace。地址 `0x2a0b92ec08` 位于可执行、
file-backed 的动态库映射内；它不是此前输出中 `sigreturn_trampoline` 的内核符号地址。

## 分析

页故障处理在故障后看到该 VPN 已有有效映射，因此不会再走按需加载；fetch fault 也不是
COW/write-protect 的可修复类别。VMA 元数据宣称 `R|X|U`，页面已驻留，故“容量满后页面未
安装”不是这两次 fault 的直接解释。

但原有 `translate()` 只返回 PPN，不能判断硬件叶子 PTE 是否缺少 `X`、`U` 或 `A` 位，也
不能判断当前 hart 的 `satp` 是否正使用该进程的页表。信号 frame 中的 restorer 地址仅用于
在 handler 返回时执行 `rt_sigreturn`；本次原始 fetch PC 已由 trap 日志单独记录，不能把
backtrace 中的 restorer 地址当作原始故障地址。

## 修复

新增默认关闭的 Cargo feature `fault-diagnostics`：

- 仅在普通用户缺页处理失败、即将发送 `SIGSEGV` 或 `SIGBUS` 时输出一次快照，包含 hart、
  PID/TID、trap cause、`stval`/`sepc`/`sp`、signal、当前 `satp` token，以及 VMA 状态。
- 快照同时输出映射 PPN、地址空间 token 和实际叶子 PTE raw flags。RISC-V 保留
  `V/R/W/X/U/G/A/D/COW` 所在的低位；LoongArch 也提供对应原始 flags，保证 feature 两架构
  均可编译。
- `SIGSEGV` 使用自定义 handler 时额外记录 handler、`SA_RESTORER`、选择的 restorer、
  signal-frame SP 和 alt-stack 状态。原始故障 PC 由 trap 行记录，frame 行明确标作
  `handler_sepc`，避免语义混淆。

完整 BuildStorm 复现使用：

```bash
make TARGET_ARCH=riscv64 special_make \
  KERNEL_EXTRA_FEATURES=perf,file-cache-capacity-test,fault-diagnostics
timeout 240s make run TARGET_ARCH=riscv64 > /tmp/rustc-fault-diagnostics-riscv.log 2>&1
rg -a -n -C 3 '\[fault-diagnostics\]|rustc interrupted by SIGSEGV' \
  /tmp/rustc-fault-diagnostics-riscv.log
```

## 验证

- 初版诊断的 240 秒 RISC-V QEMU 已复现两次上述 fetch fault，并确认日志在发送同步信号前写出。
- 包含 `fault-diagnostics` 的 `special_make` 已在 RISC-V 与 LoongArch64 编译通过。
- 默认 `make perf TARGET_ARCH=riscv64` 和 `make perf TARGET_ARCH=loongarch64` 通过；该
  feature 默认关闭，正常缺页路径不扫描 VMA 或打印日志。
- 加入 PTE/token 字段后的 180 秒 RISC-V 运行只到 `BUILDSTORM_TOOLCHAIN ok`，没有进入 Cargo
  编译/故障窗口，故尚无新字段的真实 fault 样本，也未声称根因或完整 BuildStorm 通过。

下一次取得 PTE flags 后：缺 `X/U` 优先检查 mmap/mprotect 的页表更新；PTE 正确但 token
不一致或跨 hart 复现则检查地址空间切换和 TLB shootdown；两者均正确时再追文件页内容、跳转目标
或 Rust runtime 的 signal handler。
