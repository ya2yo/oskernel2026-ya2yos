# LTP mmap3 并发临时文件缓存抖动与动态栈回收

## 背景

LTP `mmap3` 在每轮中创建临时文件，立即 `unlink`，由 40 个线程并发写入最多约 4 MiB 的随机数据，再执行 `fsync`、`mmap(MAP_SHARED)`、`memset`、`munmap` 和 `close`。Ya2yOS 的 ext4 适配层使用全局 `EXT4_OP_LOCK` 串行化 lwext4 操作，并为普通文件维护一个 whole-file write-back cache；该缓存的 FIFO 容量为 `FIFO_SIZE = 10`。

## 现象

原始 `log.ans` 中没有 `TFAIL`、`panic`、`ERROR` 或 `WARN`，但 musl 和 glibc 的 `mmap3` 都在 LTP 90 秒 watchdog 到期时输出：

```text
TBROK: Test killed! (timeout?)
passed   0
failed   0
broken   1
```

LTP 包装器返回的 `512` 是 `LTP_TBROK << 8` 的 wait status，不是测试断言失败。debug 日志中同一个临时 `ashfile*` 路径重复出现几十次 `initialize cache!`，说明活跃文件的缓存被反复淘汰和重建。

## 分析

每个临时文件首次写入时都会建立缓存。40 个文件同时活跃时，前 10 个缓存占满 FIFO；后续文件插入会把较早但仍在写入的文件写回并移出 `FIFO_TABLE`。这些文件下一次 `file_seek()` 时已经不在 `CACHE_TABLE`，`check_cached()` 重新执行 `ext4_fopen`/`ext4_fread` 并创建缓存。新缓存又淘汰另一个活跃文件，形成缓存抖动。每次淘汰、重建和写回都要经过全局 ext4 锁，最终把本应由内存缓存完成的随机小块写入放大成串行 lwext4 I/O。

尝试对延迟删除文件直接禁用缓存后，重复初始化消失但 4 KiB 直写仍不足以在 watchdog 内完成，因此不能只靠绕过缓存解决问题。

排查过程中还发现两个会放大或掩盖问题的语义缺口：

1. `file_size()` 曾用 `ext4_fopen(O_RDONLY)` 覆盖当前活动的 `self.file_desc`，但保留原来的 `last_flags`。逻辑上仍是 `O_RDWR` 的文件后续可能实际以只读句柄执行写入。
2. `mmap(MAP_STACK)` 创建的动态 pthread 栈被标记为 `MapAreaType::Stack`。`munmap()` 原来只回收 `MapAreaType::Mmap`，所以动态栈的页表、物理页和 `total_mmap_size` 不会释放；固定进程主栈同样是 `Stack`，不能简单地把所有 Stack 区域都纳入回收。

## 根因

根本性能问题是 write-back FIFO 只表达“最近缓存项”，没有表达延迟删除 inode 仍有活动 fd 的生命周期。`mmap3` 的 40 个临时文件都处于该状态，FIFO 容量不足导致 active cache eviction/rebuild，在全局锁下持续串行。另一个独立的内存管理问题是 `munmap()` 没有把带 `MAP_STACK` 的动态栈识别为可回收 VMA。

## 修复

- `crates/lwext4_rust/src/file.rs` 为 `Ext4File` 增加 `cache_pinned`。延迟删除 inode 在 `write_at()` 中调用 `pin_write_back_cache()`：从 FIFO 移除路径但保留 `CACHE_TABLE` 缓存，后续缓存初始化直接插入缓存表而不参与普通 FIFO 淘汰。最后一个 inode 引用释放时仍通过原有 `file_remove()`、写回和缓存清理路径回收状态。
- 同文件让成功的 `write_back_cache_entry()` 清除 `modified`，避免已由 `fsync` 写回的缓存继续被视为脏缓存；`file_size()` 改用独立的只读 `ext4_file` 描述符，不再覆盖活动描述符的 flags、位置或句柄状态。
- `os/src/fs/ext4_lw/inode.rs` 在延迟删除文件写入前固定其缓存，确保临时文件在整个 fd 生命周期内不被 FIFO 抖动驱逐。
- `os/src/mm/memory_set/mmap_ops.rs` 增加动态栈判断：只有 `MapAreaType::Stack` 且带 `MAP_STACK` 的区域按 mmap VMA 处理并允许 `munmap()` 回收；固定主栈仍被排除。

## 验证

- 基线 `timeout 120s make run > /tmp/mmap3-release-baseline.log 2>&1` 在 watchdog 内未完成，musl/glibc 均出现 `TBROK`。
- `timeout 300s make run > /tmp/mmap3-pinned-300.log 2>&1` 独占 QEMU 回归通过：musl 与 glibc 各输出 `mmap3.c:124: TPASS`，summary 均为 `passed 1 failed 0 broken 0`，结果码均为 0，日志末尾为 `shutdown!`，无 `panic`、`ERROR` 或 `WARN`。
- `make TARGET_ARCH=riscv64` 通过，当前根 Makefile 同时完成 RISC-V 与 LoongArch64 release 构建。
- `make log TARGET_ARCH=riscv64` 通过，debug logging 配置可正常构建。
- `git diff --check` 通过。

未运行完整 LTP 批量套件；本次验证聚焦于 `mmap3` 触发路径，避免把单项回归结果扩大解释为全量测试结论。
