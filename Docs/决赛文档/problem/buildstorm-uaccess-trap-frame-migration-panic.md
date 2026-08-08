# BuildStorm RISC-V uaccess trap frame 迁移导致取指 panic

## 背景

P3 为短小用户缓冲区增加了 RISC-V/LoongArch64 的直接 uaccess。直接访问用户页时，
架构 trap frame 可以把缺页交给 `MemorySet::handle_page_fault()`，或跳到 copy
helper 的 fixup。

## 现象

`server.ans` 在 CAgent 启动后报告两次 `Exception(FetchInstructionPageFault)`，
`sepc == stval`（例如 `0x2fde8`、`0x2aeac`），随后在
`trap_from_kernel_frame` panic。`client.ans` 的多个 Hart 同时位于用户 exec/缺页和
内核 trap 路径。临时 CSR 诊断显示 fault PC 是用户地址，但 trap frame 的 `sp` 已是
内核栈，`sstatus.SPP=0`、`sscratch=0`，说明用户返回现场已被破坏，而不是 fault 地址
对应的内核代码无效。

## 分析

`trap_from_kernel_frame()` 运行在架构同步 trap 的保存现场中。原实现对缺页直接调用
完整 `MemorySet::handle_page_fault()`；文件页 demand fault 可能进入 EXT4/磁盘等待并
调度当前任务。任务切出后可以在另一 Hart 恢复，但 `sepc`、`sstatus`、`sscratch`
仍只保存在原 Hart 的 CSR 中，`TaskContext` 不会保存这组 trap CSR。恢复后执行
`sret` 使用了不匹配的架构状态，最终把用户取指 fault 误当成 kernel fault 并 panic。

## 根因

可阻塞的缺页修复被放进了不能跨 Hart 迁移的同步 kernel trap frame。该路径违反了
trap CSR 与任务上下文的所有权边界；它不是 CAgent 用户程序本身的非法取指。

## 修复

- `os/src/mm/uaccess.rs` 的 kernel fault handler 不再调用可能阻塞的
  `MemorySet::handle_page_fault()`。
- 仅对已存在且权限匹配的 PTE 做每 Hart 一次本地 TLB 失效重试；缺页、COW、文件页和
  无效指针统一跳到 copy helper fixup。
- fixup 返回后，`copy_from_user`/`copy_to_user` 使用已有软件翻译路径，在普通 syscall
  上下文中完成可阻塞的缺页、COW 和文件页处理。

## 涉及文件

- `os/src/mm/uaccess.rs`

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过，RISC-V release 内核和用户程序构建通过。
- `git diff --check`：通过。
- 修复后的 RISC-V qcow2 overlay 运行 180 秒：`sigaltstack regression: PASS`、
  `rseq regression: PASS`，CAgent 十项全部通过并进入 BuildStorm，输出
  `BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok`，推进到 `443/446`；日志未再出现
  `FetchInstructionPageFault`、kernel panic、`TFAIL` 或 `TBROK`。完整 BuildStorm END 尚未
  出现。
