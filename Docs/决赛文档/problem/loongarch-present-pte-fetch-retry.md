# LoongArch BuildStorm 有效用户页取指 fault 重试

## 背景

`server.ans` 的修改时间为 2026-08-06 11:09:42，晚于 FCC 上下文修复提交
`cd5896c6`（01:05:07）。因此本日志不能用 FCC 修复解释，必须按新日志重新定位。

## 现象

LoongArch64 BuildStorm 在 `core`/`compiler_builtins` 并发编译时出现：

```text
cause=Exception(FetchInstructionPageFault)
stval=sepc=0x6e2f30
pte_flags=Some(0x9d)
area_type=Mmap map_perm_bits=26 (R|X|U) resident_frame=true
```

随后 rustc 收到 SIGSEGV。另有多条取指 fault 位于用户地址空间之外的 restorer 路径，属于
信号处理后的伴随现象，不能替代第一条有效页表现场作为根因。

## 分析

LoongArch PTE `0x9d` 包含有效位、PLV3、缓存属性和物理页存在位，且没有
`UNEXECUTABLE`/`UNREADEABLE`，软件页表明确允许用户取指和读取。`handle_page_fault()` 看到
该叶子已经存在时不会重新装页，旧 trap 路径又只在 RISC-V 编译条件下执行 present-PTE 重试，
于是 LoongArch 将一次可能由并发映射或本 hart stale TLB 引起的 fault 直接投递为 SIGSEGV。

## 根因

已存在用户叶子 PTE 的 load/fetch fault 没有在 LoongArch 路径执行本地 TLB 失效和有限重试。
这是翻译缓存与软件页表暂时不一致时的错误失败路径，不是 VMA 权限缺失。

## 修复

- 在 LoongArch `PageTable` 增加 `is_user_executable()` 和 `is_user_readable()`，分别检查
  `PLV3` 与可执行/可读标志。
- 将 `MemorySet` 和 trap 层的一次性 present-PTE 重试扩展到 RISC-V、LoongArch 两架构。
- 重试前执行架构本地 `tlb_invalidate()`，取指 fault 额外执行 `instruction_fence()`；同一
  VPN 连续第二次 fault 仍发送 SIGSEGV，成功缺页或 syscall 后清理重试状态。

## 涉及文件

- `os/src/arch/loongarch64/qemu/page_table.rs`
- `os/src/mm/memory_set/handle.rs`
- `os/src/task/task/task.rs`
- `os/src/trap/mod.rs`

## 验证

- `rustfmt --edition 2021 --check`（本次四个 Rust 文件）：通过。
- `make TARGET_ARCH=loongarch64 build-arch`：通过。
- `make TARGET_ARCH=riscv64 build-arch`：通过。
- `make log TARGET_ARCH=loongarch64 KERNEL_EXTRA_FEATURES=fault-diagnostics`：通过。
- `timeout 240s make run TARGET_ARCH=loongarch64`：通过启动、`sigaltstack regression: PASS`、
  `rseq regression: PASS` 和 `BUILDSTORM_TOOLCHAIN ok`；timeout 前未出现新的
  `fault-diagnostics`、panic 或 SIGSEGV。完整 BuildStorm 尚未到达原始 Cargo 编译窗口。

