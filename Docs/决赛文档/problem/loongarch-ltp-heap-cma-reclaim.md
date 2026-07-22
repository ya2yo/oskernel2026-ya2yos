# LoongArch 批量 LTP Rust 内核堆耗尽与 CMA 扩容

## 背景

LoongArch64 pre-tests 批量运行在 LTP 尾段退出。维护者提供的根目录
`loongarch.ans` 在 `fsmount01` 启动后报告 Rust 全局分配器 OOM：

```text
[kernel] Panicked at src/mm/heap_allocator.rs:18
Heap allocation error, layout = Layout { size: 2065194, align: 1 }
```

单独运行 LTP 用例不会稳定触发，说明除了单次大对象，还需要处理长期批量运行中
未及时释放的 VFS/进程资源。

## 现象与定位

`2,065,194 = 0x1f832a`，精确等于 pre-tests 镜像中 `/musl/busybox` 的最大
`PT_LOAD` 末端：第二个 load segment 的 `p_offset=0x1f7e48`、
`p_filesz=0x4e2`，两者相加为 `0x1f832a`。`fsmount01` 会检查并执行
`mkfs.ext2`，而 `/bin/mkfs.ext2` 是 BusyBox applet，因此该次请求来自
`elf_loader.rs` 为 ELF image 创建的 `Vec<u8>`，不是 LTP 输出管道本身。

本项目的 buddy allocator 会把该 layout 向上取整为完整的 2 MiB buddy block。
原全局 Rust allocator 只管理 `.bss` 中固定的 128 MiB `HEAP_SPACE`；LoongArch
8 GiB 的两段 RAM 虽已加入 CMA，但不会自动成为 `Box`、`Vec`、`String` 使用的
全局堆。因此 128 MiB 堆只要总量耗尽或缺少一个连续 2 MiB 块，就会在正常的
BusyBox ELF 装载时 panic。

当前 LoongArch 链接布局下，内核末端到低端 256 MiB RAM 段末尾仅剩约 119 MiB
CMA，少于本修复的首选 128 MiB 扩容块；首次扩容会自然落到高端
`0x9000000080000000..0x9000000270000000` 直映 RAM。该直映区由 LoongArch DMW0
覆盖，不跨越 PCI/MMIO hole。

## 根因

直接根因是固定 128 MiB 全局 Rust 堆没有在 OOM 时接管已可用的 CMA 页，无法满足
一个合法的 2 MiB ELF 装载请求。

批量运行的放大因素包括：

- 全局 dentry/inode 查找缓存曾可随短生命周期路径持续增长；
- 进程退出时共享 `MAP_SHARED` 写回错误会阻断后续 VMA/page-table 清理；
- LTP runner 先阻塞读取输出 pipe 到 EOF，再等待 direct child。若后台 helper 继承 pipe
  写端，direct child 已退出后 EOF 不会到达，后代清理与资源回收永远不可达。

这些因素不改变 2 MiB 请求的来源，但会让静态堆更早耗尽或碎片化。

## 修复

### CMA 全局堆后备

`heap_allocator.rs` 将 global allocator 包装为 `KernelHeapAllocator`：

1. 先从原 `.bss` buddy heap 分配；
2. 失败且 `mm::init()` 已完成完整 direct map 和 CMA 初始化后，串行进入
   `CMA_HEAP_GROW_LOCK`；
3. 在不持有 `HEAP` 锁时向 CMA 申请连续、页对齐的块，优先 128 MiB，碎片化时逐次
   折半至本次 layout 需要的 buddy block；
4. 将该 CMA block 的 `KernelAddr` 用 `add_to_heap()` 永久移交给全局 heap，并在同一
   次 `HEAP` 锁持有中完成原始请求的实际分配。

最后一步避免了“探测可分配、释放探测块、其他 hart 抢走新块、当前 hart 假 OOM”的
SMP 竞态。CMA 页一旦移交不会再调用 `cma_dealloc()`，后续子分配由 global buddy heap
管理。OOM 信息现在包含 `user`、`actual`、`total` 和 `cma_backing` 统计；统计快照在
释放 heap 锁后才 panic，避免 panic 输出路径重入 allocator。

RISC-V 在 `activate_kernel_space()` 和 `init_cma_late()` 后才启用此 fallback；
LoongArch 使用高低两段真实 RAM 的 DMW 直映。两架构均不会把 MMIO hole 伪装为 heap。

### 批量回收和锁边界

- dentry 缓存限制为 4096 项，达到上限或最后一个 fd table owner 退出时清理；
- inode 缓存同样在压力/退出边界驱逐仅由 cache 自己持有的 inode；
- `Ext4Inode::drop()` 会获取 `EXT4_OP_LOCK`，所以 dentry/inode map 的替换、删除和
  reclaim 都先把最终 `Arc` 移出 cache 锁，再在锁外析构，避免 `FsIndex -> EXT4_OP_LOCK`
  反向锁链；
- `recycle_data_pages()` 按 `data_frames` 中真实连续 resident VPN 段写回 shared mmap，
  无论写回错误都清空 VMA 和页表，且始终恢复共享 open-file-description 的 offset；
- LTP runner 将 read pipe 的父端设置为 `O_NONBLOCK`，以 `waitpid(WNOHANG)` 轮询 direct
  child。direct child 被回收后立即沿用 testsuit 既有的 `kill(-1, SIGKILL)` 隔离策略并
  用 `__WALL` reap 后代，最后只 drain 已缓冲输出，不会被泄漏的 pipe writer 卡住。

用户态新增 raw wait wrapper，仅供需要区分 `EINTR`、`ECHILD` 和成功 `WNOHANG == 0`
的 LTP runner 使用；原 `wait()` / `waitpid()` 的简化 `-1` ABI 不变。

## 涉及文件

- `os/src/mm/heap_allocator.rs`
- `os/src/mm/mod.rs`
- `os/src/fs/dcache.rs`
- `os/src/fs/kernel_fs_ops/fsidx.rs`
- `os/src/fs/fstruct.rs`
- `os/src/fs/mod.rs`
- `os/src/mm/memory_set/accessors.rs`
- `os/src/task/mod.rs`
- `user/src/bin/ltp/mod.rs`
- `user/src/lib.rs`
- `user/src/syscall/mod.rs`

## 验证

已执行：

```text
rustfmt --edition 2021 <modified Rust files>
git diff --check
make loongarch64-build
make riscv64-build
```

两种 release 构建均通过。LoongArch64 pre-tests 单例 `fsmount01` 实机日志
`/tmp/loongarch-fsmount01-cma.log` 已确认：

- `mm:heap CMA backing enabled` 出现；
- BusyBox `mkfs.ext2` 路径实际执行；
- 6 项 `TPASS`，没有 `Heap allocation error`、`CMA OOM` 或 kernel panic；
- 测例仍输出 `TBROK: Test 6 haven't reported results!`，Summary 为
  `passed 6 failed 0 broken 1`，这是当前环境残留的 testcase 结果，未被标记为本次已修通；
- QEMU 正常输出 `shutdown!`。

另一次完整 `get_score()` 启动日志 `/tmp/loongarch-heap-cma-final.log` 覆盖到网络和
BusyBox 前段，已看到 CMA 后备启用且无 OOM/panic；运行环境在约 128 秒处外部终止，未进入
LTP，不能据此宣称批量 `fsmount01` 已完整回归通过。

## 边界

当前 128 MiB 首选块在现有 LoongArch 链接布局下会落到高 RAM。若未来内核显著缩小、低端
CMA 也能提供完整 128 MiB，单一 CMA buddy allocator 的精确取址仍由 free-list 状态决定；若
需要把“必须优先高段”作为长期 API 保证，应将 CMA 拆分为可选择范围的 allocator，而不是
继续依赖当前布局。
