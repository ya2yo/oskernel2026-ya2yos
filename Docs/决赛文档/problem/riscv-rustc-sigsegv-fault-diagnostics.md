# RISC-V BuildStorm rustc SIGSEGV 缺页现场诊断

## 背景

RISC-V BuildStorm 的长日志先后将 `find-msvc-tools`、`unicode-ident` 报为
`rustc ... (signal: 11, SIGSEGV)`。崩溃 crate 会随并发时序变化，因而只是受害进程，
不能据此把问题归因于某个 Cargo 依赖，也不能用 `RUST_MIN_STACK` 掩盖。

前一轮文件页缓存满载处理已经改为容量旁路，并将连续 `in_use` 候选移出热扫描路径；
该路径的统计正常，但仍需要取得发生 SIGSEGV 时的实际页表和信号现场。

## 现象

启用完整诊断的 RISC-V BuildStorm 复现，在 Cargo `Building 0/446` 和 `1/446` 时分别捕获：

```text
hart=2 pid=115 tid=124 stval=sepc=0x2a0b92df9a
active_page_table_token=0x90725 memory_set_token=Some(90725)
pte_flags=Some(5b) mapped_ppn=Some(681522)

hart=3 pid=116 tid=159 stval=sepc=0x2a0b8caa60
active_page_table_token=0x90994 memory_set_token=Some(90994)
pte_flags=Some(5b) mapped_ppn=Some(713756)
```

两次 VMA 均为 `Mmap/Framed`、逻辑权限 `R|X|U`、file-backed 且 resident。随后 Rust 的
SIGSEGV handler 被调用并打印 backtrace；原始 fetch PC 是上述 `stval/sepc`，而不是 backtrace
中出现的 `sigreturn_trampoline` restorer 地址。

## 分析

`0x5b` 即 `V|R|X|U|A`。同时，触发 fault 的 hart `satp` token 与该进程 `MemorySet` token
相同。因此这两次 fault 不是容量旁路导致的未映射页、不是叶子 PTE 缺少 `X/U`，也没有证据表明
执行了错误的地址空间。页故障处理在故障后看到该 VPN 已有有效映射，故不会再走按需加载；fetch
fault 也不是 COW/write-protect 的可修复类别。

RISC-V 的 PTE 更新需要在重试访问前以 `sfence.vma` 使本 hart 的地址翻译失效。源码中
`PageTable::activate()`、COW、`mprotect`、`munmap` 都已有本地 TLB 刷新，但
`handle_mmap_read_page_fault()` 在安装新叶子 PTE 后直接返回，
`handle_mmap_write_page_fault()` 在修改权限后也未刷新；匿名 `lazy_page_fault()` 的 `map_one()`
路径同样缺失。一次取指缺页可以先安装正确 PTE，随后重试却仍命中硬件缓存的 non-present
translation；第二次进入内核时软件页表已是 `0x5b`，于是被误判为不可恢复并向 rustc 投递
`SIGSEGV`。这与两份现场完全吻合。

该问题不需要 remote TLB shootdown：调度器仍限制同一 `MemorySet` 固定在一个 `home_hart`，本修复
只处理发生 fault 的当前 hart。signal frame 中的 restorer 仅用于 handler 返回时执行
`rt_sigreturn`，不能作为原始故障地址或根因。

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

基于上述现场和调用路径，补齐缺页成功路径的本地 TLB 刷新：

- RISC-V `handle_mmap_read_page_fault()` 在安装 file/shared PTE 后执行 `sfence.vma`；
  `handle_mmap_write_page_fault()` 在更新 COW/dirty/permission 后同样刷新。
- 通用 `lazy_page_fault()` 在 `map_one()` 成功后刷新，覆盖 ELF BSS、brk 与栈等匿名懒映射。
- LoongArch 的 file mmap read 安装路径同步执行既有 `tlb_invalidate()`，维持两架构的缺页重试语义。

没有改动 `MAX_FILE_PAGE_CACHE_PAGES`，没有设置 `RUST_MIN_STACK`，也没有把本地刷新扩散到
进程创建期间的批量映射路径。

完整 BuildStorm 复现使用：

```bash
make TARGET_ARCH=riscv64 special_make \
  KERNEL_EXTRA_FEATURES=perf,file-cache-capacity-test,fault-diagnostics
timeout 240s make run TARGET_ARCH=riscv64 > /tmp/rustc-fault-diagnostics-riscv.log 2>&1
rg -a -n -C 3 '\[fault-diagnostics\]|rustc interrupted by SIGSEGV' \
  /tmp/rustc-fault-diagnostics-riscv.log
```

## 验证

- 诊断版日志已复现两次上述 fetch fault，并确认日志在发送同步信号前写出。
- `cargo fmt --manifest-path os/Cargo.toml --all -- --check`、`git diff --check` 通过。
- 默认 `make TARGET_ARCH=riscv64`（同时构建 RISC-V、LoongArch64 release）和
  `make TARGET_ARCH=riscv64 special_make KERNEL_EXTRA_FEATURES=perf,file-cache-capacity-test,fault-diagnostics`
  通过。
- 修复后的 120 秒 RISC-V QEMU 冒烟通过 `BUILDSTORM_TOOLCHAIN ok` 与
  `BUILDSTORM_MINIBUILD ok`，timeout 前没有 `fault-diagnostics`、rustc `SIGSEGV`、`panic`、
  `TFAIL` 或 `TBROK`。该样本尚停在 `pre-build tg-xtask`，未进入 Cargo 编译段，不能替代完整
  BuildStorm 或证明长程随机错误已完全消失。

后续应在固定镜像和参数下完成至少一次完整 BuildStorm；若仍出现 PTE 为 `V|R|X|U` 的 fetch fault，
再记录 fence 前后的首次/重试 fault 次数，并审查所有非缺页的当前地址空间 PTE 更新路径。

## 后续修复：已存在用户页的 load/fetch fault 重试

### 背景

前一阶段为按需映射成功路径补充了本地 TLB 刷新，但多线程 rustc 仍可能在另一个线程安装同一页的过程中
先取得缺页现场。缺页处理重新取得地址空间锁后，软件页表已经有有效叶子 PTE，因而不会再次加载页面；若陷入原因为
普通用户 load，旧代码只对 instruction fetch 提供一次重试，load fault 会直接进入 `SIGSEGV` 分支。

### 根因

RISC-V 的当前 hart 可能仍缓存安装前的 non-present translation。软件页表中的 `V|R|U` 或 `V|R|X|U` 叶子并不代表
本 hart 的地址翻译已经更新；没有在 fault 返回前失效该 VPN 的 TLB 时，重试仍会再次触发相同 fault。原有
`instruction_fault_retry` 只覆盖取指，无法覆盖同一竞态下的普通读取。

### 修复

- 在 RISC-V `PageTable` 和 `MemorySet` 增加 `is_user_readable()`，只接受同时具备 `R` 与 `U` 的有效叶子 PTE。
- 将 task 内的一次性重试状态改名为 `present_page_fault_retry`，同时覆盖 `LoadPageFault` 与
  `FetchInstructionPageFault`；同一 VPN 的连续第二次 fault 仍发送 `SIGSEGV`，避免真实错误进入无限重试。
- 重试前执行本地 `tlb_invalidate()`；只有取指 fault 额外执行 `instruction_fence()`。成功处理缺页或进入 syscall 时清除
  重试状态，避免状态泄漏到下一次独立访问。

### 涉及文件

- `os/src/arch/riscv64/qemu/page_table.rs`
- `os/src/mm/memory_set/handle.rs`
- `os/src/task/task/task.rs`
- `os/src/trap/mod.rs`

### 验证边界

本轮依据暂存区补写文档，未重新编译内核或启动 QEMU；暂存区没有附带新的 fault 计数或完整 BuildStorm 结束标记。
此前问题记录中的诊断版构建与 120 秒冒烟结果仍只证明前一阶段成功路径 fence，不能替代本次 load/fetch 重试的定向回归。
