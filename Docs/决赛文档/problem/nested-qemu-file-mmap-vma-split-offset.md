# 嵌套 RISC-V QEMU 文件映射 VMA 切分偏移错误

## 背景

final-2026 RISC-V 镜像中的 `initproc` 通过镜像自带动态加载器启动
`/opt/qemu-rv64/bin/qemu-system-riscv64`，再运行
`arceos-helloworld`。本问题要求保留该手工动态加载器启动方式，直接修复内核
虚拟内存语义。

## 现象

前置 `fstat unlink`、`sigaltstack`、`rseq` 和 `uptime` 回归均通过，但嵌套
QEMU 启动过程中发生：

```text
cause=Exception(FetchInstructionPageFault)
stval=0xc00000fe
sepc=0xc00000fe
arceos qemu exited with status: 139
```

故障现场的返回地址为 `0x2a22b0e624`。按 QEMU 10.0.11 PIE load bias
`0x2a223c9000` 换算为 ELF 偏移 `0x745624`，反汇编为：

```text
0x74561a: ld   a5,8(s0)
0x74561c: beqz a5,0x745604
0x74561e: mv   a1,s1
0x745620: mv   a0,s4
0x745622: jalr a5
0x745624: bne  a0,s5,0x745604
```

因此 `sepc` 不是内核截断的程序计数器，而是 QEMU 从 CSR 回调表读出的错误
函数指针目标。RISC-V `jalr` 会清除目标最低位，原始坏值为
`0x00000000c00000ff` 时，实际取指地址正是 `0xc00000fe`。

## 分析

QEMU 的 `csr_ops` 位于其可写 PT_LOAD 段。动态加载器将该段映射为：

```text
VA     [0x2a2311d000, 0x2a23415000)
file offset = 0xd54000
MAP_PRIVATE | MAP_FIXED | MAP_DENYWRITE
```

随后它执行：

```text
mprotect(0x2a2311d000, 0x1ce000, PROT_READ)
```

该操作把 VMA 分为前半只读区域和从 `0x2a232eb000` 开始的后半可写区域。旧实现
只改变后半 VMA 的 `vpn_range`，却保留了旧的 `mmap_file.offset=0xd54000`。因此，
对后半区域的惰性文件缺页不再满足：

```text
file_offset = (fault_va - vma_start) + vma_file_offset
```

目标 `csr_ops[0x57].read` 对应的虚拟地址为 `0x2a232fe518`。错误 VMA 元数据使它
从文件偏移 `0xd67518` 读取；提取的实际 QEMU ELF 在该位置的 64 位值为：

```text
0xd67518: 00000000c00000ff
```

正确的后半 VMA 文件偏移应为 `0xf22000`，目标读取位置应为 `0xf35518`。错误页
内容被当成回调地址，最终由 `jalr` 跳至 `0xc00000fe`。

这也解释了为何调整 QEMU 内存、CPU 或加载器参数可能改变复现时机，却不能修复
问题：错误发生在内核维护的文件映射 VMA 元数据中。

## 根因

`MemorySetInner::mprotect()` 在切分 VMA 时没有为起始地址右移的片段同步增加
`mmap_file.offset`。`MemorySetInner::munmap()` 的“保留右半段”与“中间打洞后生成右
半段”路径存在同一类缺陷。后续按需装页据此读取了错误的文件页。

## 修复

在 `os/src/mm/memory_set/mmap_ops.rs` 中，为每个起始地址右移的 VMA 片段增加：

```text
(new_start_vpn - old_start_vpn) * PAGE_SIZE
```

到 `mmap_file.offset`：

- `munmap` 保留右半段，以及从中间移除范围后创建的右侧 VMA；
- `mprotect` 生成的右侧片段、原地保留的右侧片段，以及三段切分中的中段和后段。

左侧片段的虚拟起点未变化，文件偏移保持不变。修改不涉及 QEMU 命令行、镜像文件、
动态加载器或 COW 语义；仅恢复惰性文件映射的页号到文件页号对应关系。

## 涉及文件

- `os/src/mm/memory_set/mmap_ops.rs`
- `Docs/决赛文档/problem/nested-qemu-file-mmap-vma-split-offset.md`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

- `make log TARGET_ARCH=riscv64`：通过。构建仅出现已有的 `.cargo/config`、
  `smoltcp` 和 `lwext4_rust` 警告。
- 维护者提供的本次 RISC-V 运行输出 `log.ans`：四项前置回归均为 `PASS`；保留手工
  动态加载器的 QEMU 10.0.11 探测和启动流程后，嵌套 guest 输出 `OpenSBI v1.6`、
  `Hello, world!`、`shutdown!`。
- 对 `log.ans` 过滤 `FetchInstructionPageFault`、`0xc00000fe` 和 `status: 139`：均无
  匹配。

本轮未重新运行 LoongArch64 构建或运行回归；该修复位于共享 VMA 元数据代码，仍应在
后续 LoongArch64 回归中覆盖。
