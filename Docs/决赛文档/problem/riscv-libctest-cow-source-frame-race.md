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

在 `os/src/arch/riscv64/qemu/page_table.rs` 中，处理函数现在在 `unmap_one()` 前：

1. 读取原始 `Arc::strong_count` 以保留 COW 的共享判断；
2. 从 `vma.data_frames` 克隆 `source_frame: Arc<FrameTracker>`；
3. 用原 PTE 的 `src_ppn` 在新页映射完成后复制内容；
4. 仅在 `copy_from_slice()` 完成后释放 `source_frame`。

这样，即使两个 hart 都删除自己 VMA 的引用，复制中的源帧仍有局部强引用，不能被 frame
allocator 回收或复用。`refcnt == 1` 分支仍按克隆前的计数直接恢复可写权限，不会被临时
pin 错判为共享页。

## 涉及文件

- `os/src/arch/riscv64/qemu/page_table.rs`
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
```

RISC-V release 构建通过。最终 QEMU 输出直接保存在仓库根目录 `log.ans`，配置为双 hart；
其中 static 组 107 项、dynamic 组 110 项，共 217 个 `START`/217 个 `END`，并出现：

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
  进入复制分支，但该边界的物理页所有权需要单独建模和验证。
- `os/src/mm/memory_set/area_ops.rs` 中临时 `PageTable::from_token()` 创建中间页表帧的
  所有权风险与本问题独立，本次没有修改或宣称已修复。
- 本次代码仅位于 RISC-V 页表实现；LoongArch64 未运行行为回归。
