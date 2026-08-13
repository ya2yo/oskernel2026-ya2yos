# LoongArch BuildStorm trap 边界全 TLB 失效

## 背景

LoongArch64 BuildStorm 在 `Compiling arceos-helloworld v0.1.0` 后长期没有新日志。
将 QEMU 降为 `-smp 1 -m 8G` 后仍可复现，因此多 Hart 调度、remote-TLB ACK、任务迁移和
大内存宿主压力不是该现象的必要条件。

维护者保留的 GDB 会话最终能够以 `Ctrl-C` 中断目标，停在
`0x9000000000203000 in srfill ()`，返回地址为用户态
`0x0000002a1859dd00`。`kernel-la` 的符号和反汇编确认该地址是
`__tlb_rfill`，其执行 `lddir`、`ldpte`、`tlbfill` 后 `ertn`，而不是普通 Rust
函数或 futex 等待点。

## 现象

在 BuildStorm 之前的 GDB 记录中，trap handler 的 syscall 断点和异常断点已分别命中数千和数万次。
`ReadLinkat` 等频繁系统调用使用户态和内核态反复切换；当每个切换都清空 local TLB 时，随后对
用户代码、栈、动态链接器和 mmap 文件页的首次访问都会重新进入 `srfill`。

在 QEMU TCG 下，软件 refill 的密度足以让串口日志、QEMU monitor 和 GDB 中断表现为长时间无响应。
这解释了日志停在某个 Cargo crate 进度行、而 GDB 抓到 TLB refill 的组合；该进度行并不表示
`arceos-helloworld` 源码本身死循环。

## 分析

LoongArch 的 `__tlb_rfill` 采用 Linux 同类的三级页表 walker：从 `CSR.PGD` 取得根，依次
`lddir 2`、`lddir 1`、`ldpte` 后 `tlbfill`。它本身没有回跳或自旋分支。用户地址
`0x2a1859dd00` 位于本内核低半用户虚拟地址范围，适合作为 refill 返回 PC。

问题出在其调用频率：

- `__alltraps` 进入 Rust trap handler 前无条件执行 `invtlb 0x0`；
- `__return_to_user` 在恢复用户寄存器前再次无条件执行同样的全 TLB 失效；
- `PageTable::activate()` 即使 `PGDL/PGDH` 已是当前任务的同一根页表，也无条件全失效。

此内核未使用 ASID，因此真正切换根页表时必须失效旧翻译；但 syscall、普通异常和同一任务的
`trap_return()` 并没有改变根页表。原实现把这些安全的同根返回也变成了全量 translation-cache
失效，导致每一个高频 syscall 至少触发一次完全可避免的 TLB refill 波。

## 根因

LoongArch trap 边界与同根 `PageTable::activate()` 的无条件全 TLB 失效造成 translation-cache
thrashing。在 BuildStorm 编译器的高 syscall/page working-set 压力下，这个性能退化表现为长期无
日志推进和 QEMU 调试控制路径饥饿，而非软件页表 walker 内存在显式死循环。

## 修复

- 删除 `__alltraps` 与 `__return_to_user` 的无条件 `invtlb 0x0`。
- `PageTable::activate()` 比较 `PGDL/PGDH` 与目标根页表，只在根页表确实改变时执行本地
  `tlb_invalidate()`。
- 保留所有改变 PTE 内容的显式失效路径：map/unmap、COW、PageModifyFault、mprotect、缺页、
  remote-TLB shootdown 及远端 mailbox ACK 均仍会失效 TLB；故不会把旧翻译带入发生实际映射
  修改或地址空间切换的场景。

## 涉及文件

- `os/src/arch/loongarch64/qemu/asms/trap.S`
- `os/src/arch/loongarch64/qemu/page_table.rs`

## 验证

- `kernel-la` 离线符号与反汇编确认 `0x9000000000203000` 是 `srfill/__tlb_rfill`，且其路径为
  `lddir -> lddir -> ldpte -> ldpte -> tlbfill -> ertn`。
- `make TARGET_ARCH=loongarch64` 通过；该顶层目标实际完成 RISC-V 和 LoongArch64 release 构建。
- `git diff --check` 通过。
- 对重新生成的 `kernel-la` 反汇编确认 `__alltraps` 与 `__return_to_user` 已无该两处
  `invtlb`。
- 未运行新的 QEMU/BuildStorm：维护者明确要求保留当前 GDB/QEMU 现场，不能停止、重启、detach
  或删除已有断点。因此本轮不宣称完整 BuildStorm 已通过；需在维护者允许建立独立实例后复测。
