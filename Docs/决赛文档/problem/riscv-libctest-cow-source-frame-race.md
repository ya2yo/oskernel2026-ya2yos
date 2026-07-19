# RISC-V libctest 批量 fork 的 COW 源帧并发释放

## 背景

`user/src/bin/initproc.rs` 当前以 musl `libctest_testcode.sh` 作为 RISC-V
双 hart 回归入口。脚本复用一个 BusyBox shell，连续执行大量
`fork() -> execve() -> wait()`；这会让父 shell 与刚创建的子进程在不同 hart
并发处理 COW 写保护页。

维护者发现每个 libctest 测例单独执行正常，但批量执行时会随机输出
`Segmentation fault (core dumped)`，要求分析 `riscv.ans` 和 `log.ans` 并修复。

## 现象

原始 `riscv.ans` 的 static 组依次完成 `argv`、`basename`，随后直接输出：

```text
Segmentation fault (core dumped)
```

第三项 `clocale_mbfuncs` 的 `START` 标记尚未出现；动态组随后仍能继续运行。
因此它不是 `clocale_mbfuncs` 的固定断言失败，而是批量脚本中某次子进程启动前后的
时序相关破坏。单独执行该测例可以通过，也与此结论一致。

临时页故障诊断捕获到 BusyBox 在 `bad addr=0x0`、`sepc=0x1066c0` 处触发
`StorePageFault`。从测试镜像导出的 `/musl/busybox` 反汇编确认该 PC 是 musl
分配器一致性检查失败后执行的：

```asm
sb zero, 0(zero)
ebreak
```

也就是说，段错误是 BusyBox/musl 检测到 heap chunk 元数据已经损坏后的主动终止，
不是测例本身的普通空指针访问。

## 分析

RISC-V 的 `PageTable::handle_write_protect_page_fault()` 在 COW 复制分支中，旧实现
先从 PTE 取得裸的源页切片，再执行 `vma.unmap_one()`。`unmap_one()` 会从当前 VMA
的 `data_frames` 删除对应的 `Arc<FrameTracker>`。

父、子地址空间各自持有同一 COW 源帧的 `Arc`，但可在两个 hart 同时缺页。旧代码可能
发生如下交错：

```text
hart A: 从旧 PTE 取得裸 src 指针
hart A: unmap_one() 删除自己的 FrameTracker
hart B: unmap_one() 删除最后一个 VMA 引用，源物理页被回收/复用
hart A: 从已复用的 src 复制到新页
```

此时 `hart A` 仍在使用物理页号，却没有任何 `FrameTracker` 保证该页归属。被分配器复用
后的内容会写入 shell 堆页，最终让后续 malloc 的 chunk 检查失败。崩溃位置随调度漂移的
现象与这一跨地址空间 COW 生命周期窗口一致。

## 根因

根因是 RISC-V COW 写保护处理在复制源页之前丢失了源帧所有权：裸页地址不能阻止
`FrameTracker::drop` 回收物理页。单地址空间写缺页会被同一 `MemorySet` 写锁串行化，
但父、子进程拥有不同的地址空间锁，必须额外固定共享物理页的生命周期。

## 修复

第一轮修复在 `unmap_one()` 前取得 `source_frame: Arc<FrameTracker>`，使源页在复制期间
不能被回收。随后按 Linux 7.0 的顺序进一步收敛为：

1. 先读取原始 `Arc::strong_count`，再克隆 `source_frame`，避免临时 pin 影响独占页判断；
2. 对非 COW PTE 区分“已可写、仅需补 DIRTY”与“真正只读、应返回 SIGSEGV”，不再把只读
   ELF 或只读映射错误升级为可写页；
3. 在源帧 pin 存活时先 `FrameTracker::alloc()` 并复制源页；若 OOM，原 PTE 和原 VMA
   frame 保持不变；
4. 仅在新页准备完成后替换当前 PTE、执行本 hart `sfence.vma`，再替换
   `vma.data_frames[vpn]` 并释放旧 VMA 引用和临时源 pin；
5. RISC-V `copy_to_user()` 对已 present 的 COW PTE 显式触发 StorePageFault 处理，避免
   内核通过裸 PPN 直接写入父子共享页而绕过硬件写保护。

这使 COW 复制不再破坏 OOM 时仍有效的旧映射，也保证两个 hart 都删除自身 VMA 引用时，
复制中的源帧仍有局部强引用。`refcnt == 1` 分支继续按克隆前的计数直接恢复可写权限。

另外修复了两个会制造或放大无所有权 PTE 的路径：

- `grow()` 缩小 Brk 时按旧 `[new_vpn, old_end)` 全范围清除 PTE 并移除已有 frame，而不是
  只遍历 `data_frames`；
- `lazy_clone_area()` 直接使用所属的 `self.page_table` 分配中间页表页，不再让
  `PageTable::from_token()` 临时对象持有可能新建的 Sv39 中间页表 frame。

## Linux 7.0 对照

本轮只读分析的 Linux 源码版本为 7.0.0（`Makefile:2-4`）。Linux 的
`do_wp_page()` 在持 PTE lock 确认 fault PTE 后，对必须复制的旧 folio 执行
`folio_get()`，释放 PTE lock 后进入 `wp_page_copy()`（`mm/memory.c:4149-4241`）。
`wp_page_copy()` 的注释明确要求“旧页已引用”；它先分配/复制新 folio，重新取得 PTE lock
并以 `pte_same()` 重验，随后先 clear/flush 旧 PTE、安装新 PTE，最后才降低旧页 rmap 并
`folio_put()`（`mm/memory.c:3741-3895`）。

Ya2yOS 当前的 `Arc<FrameTracker>` 是这一 `folio_get()` 生命周期 pin 的等价物，
`MemorySet` 写锁和“同一地址空间固定一个 hart”约束则替代了 Linux 的部分 PTL/远程 TLB
并发条件。本轮没有照搬 Linux 的全局 `struct page`、rmap、`mm_cpumask` 或 SBI/IPI
shootdown：当前 `tlb_invalidate()` 仅刷新本 hart，只有维持既有 `home_hart` 约束时才正确。

## 涉及文件

- `os/src/arch/riscv64/qemu/page_table.rs`
- `os/src/mm/translate.rs`
- `os/src/mm/memory_set/area_ops.rs`
- `Docs/决赛文档/problem/riscv-libctest-cow-source-frame-race.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

执行：

```bash
make TARGET_ARCH=riscv64 build-arch
timeout 90s make TARGET_ARCH=riscv64 run > log.ans 2>&1
make TARGET_ARCH=loongarch64 build-arch
```

RISC-V 与 LoongArch64 release 构建均通过。最终 RISC-V QEMU 输出直接保存在仓库根目录
`log.ans`，配置为双 hart；其中 static 组 107 项、dynamic 组 110 项，共 217 个
`START`/217 个 `END`，并出现：

```text
#### OS COMP TEST GROUP END libctest-musl ####
shutdown!
```

日志未出现 `Segmentation fault`、`core dumped`、`StorePageFault`、`LoadPageFault`、
`SIGSEGV`、`panic`、`TFAIL` 或 `TBROK`。static 和 dynamic 各有一次 `FAIL utime
[status 1]`，其 `futimens`/时间语义失败在本次修复前已存在；它们不会中断脚本，且不属于
本次堆损坏问题。

## 剩余风险

- `data_frames` 中没有对应条目的历史 Brk COW PTE 仍没有可克隆的源帧；当前代码会强制
  进入复制分支以保持历史 iozone Brk 语义，但无法从裸 PPN 取得真正的生命周期 pin。长期
  需要为每个受管 present PTE 建立可查找的 frame owner/refcount，而不能伪造
  `FrameTracker`。
- 通用 `MapArea::unmap_one()` 仍是“先移除 Arc、后清 PTE”的旧顺序；本轮 COW 分裂路径已
  不再调用它，但常规 munmap/area teardown 仍应在后续引入类似 Linux `mmu_gather` 的
  “清 PTE/TLB 后释放 frame”批处理。
- `push_with_given_frames()` 对稀疏映射仍按 VMA 起点 zip frame Vec；MAP_SHARED 的预 fault
  OOM/稀疏 PTE 需要后续改为按 `(vpn, frame)` 复制并传播 ENOMEM。
- LoongArch64 完成了构建验证，未运行本轮 COW 行为回归；若未来允许同一地址空间跨 hart
  运行，必须先实现远程 TLB shootdown。
