# BuildStorm EXT4 inode 级写回缓存并发

## 背景

BuildStorm 在 8 hart RISC-V 上并行运行 Rustc，会交错创建、写入、读取、rename 和
`fstat` 大量小文件。Ya2yOS 的 `lwext4_rust` 后端共享一个非 SMP-safe 的块缓存，过去所有
`Ext4Inode::write_at()` 即使只是修改已存在的内存 write-back cache，也会占用
`EXT4_OP_LOCK`，从而阻塞无关 inode 的 read、lookup 和 metadata 查询。

本问题以 Linux VFS/ext4 为参照，但不假设 lwext4 已支持 Linux 那样的并发块分配与页缓存。
Linux 源码对照记录位于
`/home/ya2yo/learning_linux/ya2yos-ext4-cache-concurrency.md`。

## 现象

维护者提供的 `tmp_09.ans` 是 BuildStorm `compile` 的中途 RISC-V 样本，最后推进到
`Building 4/446`，没有 panic、ERROR、TFAIL、TBROK、`shutdown!` 或完整结束标记。

在 `t=69.826s -> 100.023s` 的文件产出阶段，累计值新增如下：

| 指标 | 增量 |
| --- | ---: |
| `ext4_read_lock` wait / hold | 67.441 / 5.201 s |
| `ext4_find_lock` wait / hold | 9.907 / 1.452 s |
| `ext4_fstat_lock` wait / hold | 9.025 / 2.498 s |
| `ext4_write_lock` wait / hold | 9.388 / 3.199 s |
| `write_at` open / quota / data | 885.612 / 885.371 / 1421.585 ms |

最后快照同时显示 write-back cache `hit_ops=124`、`init_ops=76`、`evict_ops=24`、
`direct_ops=17`。因此写缓存命中是有明确证据的热路径；稀疏写缓冲计数为零，不能作为本轮主目标。

## Linux 对照

Linux `struct inode` 在 `include/linux/fs.h` 维护 `i_rwsem`，`inode_lock()` 以 inode 为
粒度串行扩展写、截断与相关元数据变更。`fs/ext4/file.c` 的
`ext4_buffered_write_iter()` 在持有目标 inode 锁时调用 `generic_perform_write()`；
`mm/filemap.c` 明确不在该锁中执行同步写回，以避免扩大锁临界区。

这不能直接移植为“删除 Ya2yOS 全局 EXT4 锁”：lwext4 的 path/file API 和挂载块缓存没有
内部 SMP 保护。可安全复用的原则是：同一 inode 的状态转换由 inode 锁保护，已经驻留的内存
缓存操作不进入底层文件系统，而真正底层操作仍串行。

## 根因

原 `Ext4Inode::write_at()` 在函数开始即取得 `EXT4_OP_LOCK`。即使已满足以下条件：

- 当前路径已有 dense `VFileCache`；
- 写入处于已完成的 64 KiB quota 预留范围；
- 不会制造空洞且不会超过 16 MiB cache 上限；

仍然会在全局锁中执行缓存表查找、cache 写锁、内存复制和 FIFO 提升。并行 Cargo 任务因而在
不相关文件的纯内存写入期间等待唯一 lwext4 gate。

## 修复

1. `os/src/fs/ext4_lw/mod.rs` 抽出任务可睡眠的 `TaskMutex`。原 `Ext4OpLock` 保持相同的
   `PollSet` 单 waiter 唤醒和 perf 统计语义；新增原语不放宽 lwext4 的串行约束。
2. `Ext4Inode` 增加每 inode `write_state`。`write_at`、`truncate`、`rename`、`unlink`、
   delayed-unlink、hard link、缓存策略转换和可能恢复路径的元数据操作按
   `write_state -> EXT4_OP_LOCK` 取得锁，保证 pathname、cache policy 和 quota reservation
   对 cache-only 写稳定。
3. `quota_reserved` 移到 inode 外层的原子字段。实际更新仍在 `write_state` 保护下，原子读取
   只用于快路径拒绝尚未预留容量的扩展写；首次写和任何 ENOSPC 回滚保持原来的慢路径会计。
4. `crates/lwext4_rust/src/file.rs` 新增 `write_cached_at()`。它只对已有 dense cache 更新字节、
   持 cache 写锁后提升 FIFO；cache miss、缓存初始化、空洞、超上限、淘汰写回和 direct write
   返回原有 `Ext4File::file_write_at()` 路径。
5. delayed-unlink 文件仍强制慢路径，使既有 `pin_write_back_cache()` 逻辑继续阻止 FIFO eviction
   写回已删除路径。

## 语义与锁序

- 缓存快路径从不触碰 `Ext4File` 或任何 lwext4 C API。
- `VFileCache` 的条目写锁将字节更新与 FIFO eviction 串行；淘汰者只能观察到完整更新后的缓存。
- rename、truncate、unlink 与 `SEEK_DATA`/`SEEK_HOLE` 共享 `write_state`，不会与同 inode 的
  cache-only 写并发执行。
- 全局 slow path 仍在 `EXT4_OP_LOCK` 下执行 `ensure_open()`、quota 预留、write cache 初始化、
  sparse/direct write 和 block-cache writeback。没有改变 sparse、ENOSPC、rename、fsync/sync 或
  磁盘可见性语义。

## 验证

- `cargo fmt --manifest-path os/Cargo.toml -- --check`：通过。
- `git diff --check`：通过。
- perf 输出新增 `ext4_write_cache fast_hit_ops/fast_hit_bytes`，只计本轮绕过
  `EXT4_OP_LOCK` 的 cache-only 写，可与原有全部 `hit_ops/hit_bytes` 区分。
- 未执行 `make`、`make perf`、QEMU 或 BuildStorm：维护者已明确说明编译完成，要求不要再构建；
  同时根目录 `disk.img` 是维护者现有的未跟踪链接，`make run` 会改动它。
- `tmp_09.ans` 早于本修复且不是完整、同缓存状态的 A/B 样本。本轮不报告端到端加速比例。

下一份同配置完整日志应同时比较 wall-clock、`ext4_write_lock` wait/hold、write cache
hit/init/evict/direct 和 `read/find/fstat` 的 wait/hold，并覆盖 write-read、rename、truncate、
unlink-open-write-close、ENOSPC、`SEEK_HOLE` 与 mmap 读页回归。
