# lwext4 SMP P21.2：并发 bcache 基础设施与串行 gate 退化

## 背景

P21.1 已将 lwext4 块设备适配改为位置无关的 `read_at/write_at`，消除了共享 cursor 和并发 `&mut Disk`。
这只是多个 hart 同时提交块请求的必要条件；C 层 `ext4_bcache` 的 LBA/LRU/dirty 索引、buffer 状态和引用计数
仍按单线程实现，所有 lwext4 public API 也仍由 mount-wide `EXT4_OP_LOCK` 串行化。

本轮开始 P21.2，只建立并发 bcache 的内存所有权和等待基础设施，不撤退全局 gate，也不声称 BuildStorm 已获得
EXT4 并行吞吐。

## 现象

维护者提供的新 `log.ans` 只运行到 Cargo `Building 13/446`，没有 `BUILDSTORM_COMPILE ... ok=true`、测试组
`END` 或 `shutdown!`。最后一个 `38.161s` interval 中：

- gate 只有 `358` 次 fast acquire，`3652` 次排队，排队比例约 `91.1%`；handoff 累计等待 `133.774s`，
  `max_queue_depth=8`。
- `ext4_read_data_lock` 等待/持有为 `77.798s/3.007s`，`ext4_find_lock` 为 `19.921s/3.420s`，
  `ext4_fstat_lock` 为 `4.433s/1.666s`。
- 同窗口 scheduler dispatch 只有 `77.210ms`；`remote_enqueues=3607`、`remote_ipi_sent=1624` 表明多核调度
  已经工作，但任务随后在同一个 EXT4 gate 排队。

这些等待时间按任务累计，可以大于 interval 墙钟。该日志证明的是“多核生产请求、单 gate 消费”形成 convoy，
不是已经开放 EXT4 共享读后发生的性能下降。

## 分析

原 bcache 有以下并发阻断项：

- LBA tree、LRU tree、dirty list、`refctr/ref_blocks/lru_ctr` 没有共同的短临界区，lookup 与 eviction 可并发释放
  同一 buffer。
- 两个任务同时 miss 同一 LBA 时都会读盘并写入同一 `buf->data`，没有 loader 所有权和 waiter。
- dirty flush 没有 `WRITEBACK` pin；旧 `dont_shake` 是整个 cache 的布尔重入标记，不能表达每 buffer 所有权。
- `ext4_block_readbytes/writebytes` 共用 `ph_bbuf`，即使主 block request 已位置无关，非对齐 byte I/O 仍会覆盖
  另一任务的 scratch 数据。
- `bread_ctr/bwrite_ctr` 的普通自增在并发 callback 下会产生数据竞争。

因此 P21.1 后仍必须保留 `EXT4_OP_LOCK`。直接把它改成读写锁只会让上述数据竞争进入实际执行。

## 本轮实现

- 在 `ext4_bcache` 增加短时 `index_lock`，统一保护 LBA/LRU/dirty 索引和引用计数；内存分配、等待、块 I/O 与
  `end_write` callback 均在锁外执行。
- cache miss 先锁外分配，再在 index lock 下二次 lookup；竞争失败者释放预分配 buffer，保证一个 LBA 只有一个
  cache identity。
- 增加 `BC_LOADING/BC_IO_ERROR`：首个任务以 CAS 成为 loader，其他持有 buffer pin 的任务等待；加载完成或失败
  后清状态并唤醒 waiter，失败由同批 waiter 观察为 `EIO`，后续新请求可重试。
- 增加 `BC_WRITEBACK/BC_EVICTING` 状态位。flush 以 CAS 声明 writeback，buffer 在设备 I/O 期间保持引用；成功后
  在短锁内移出 dirty list，锁外执行 callback，再清状态并唤醒。
- Rust wrapper 注册 task-aware wait/wake hook：任务上下文使用 `PollSet` 阻塞，启动期无 current task 时才自旋。
  当前 hook 为全局 wake-all，正确性依赖 buffer 状态复检；按 LBA 分桶属于下一轮性能工作。
- 非对齐 byte I/O 改用每次调用的 scratch buffer，移除共享 `ph_bbuf`；块读写计数改为 relaxed atomic。
- P21.2a 将 bcache、同步 `bread/bwrite` 和 Rust `Disk::dev` 请求统计接入累计/interval perf 报告。C 统计独立于
  `struct ext4_bcache`，避免生成的 Rust binding 落后于 C 布局时扩大 `Box<ext4_bcache>` 的 ABI 风险。
- C 与 Rust 设备统计在 mount 完成后同一边界清零启用，排除 mount/recovery 初始化 I/O；普通 release 不保留
  Rust 计数对象，C 侧默认关闭，只在 perf 内核挂载成功后开启。

## 当前边界

本轮代码仍有意保留以下限制：

- `EXT4_OP_LOCK` 没有撤退，所以新日志不能用于衡量这组 P21.2 代码的并行收益。
- clean-only shake 遇到 cache 全 dirty 时还没有“升级到排他回写”的协议；当前实现可能暂时越过 `bc->cnt` 目标。
  在容量反馈完成前不得开放 shared read。
- waiter 仍是 mount 全局集合，一个 LBA 完成会唤醒其他 LBA 的 waiter；不会丢失状态复检，但会造成惊群。
- telemetry 已能观察 bcache hit/miss、loader/wait、shake/evict、writeback、驻留量和 C/Rust 请求边界，但 relaxed
  快照不是跨字段事务：一次同步请求跨越采样点时，interval 的 submit/complete 或 loader start/end 可暂差 1。
- inode identity、open-file `f_pos`、目录、allocator 和 journal 仍是串行语义，metadata 并发不在本轮范围。

## P21.2a telemetry 结果

维护者提供的正式 `log.ans` 运行到约 `573.420s`、Cargo `Building 21/446`，有
`BUILDSTORM_TOOLCHAIN/MINIBUILD ok`，没有 panic、ERROR、TFAIL 或 TBROK；但没有
`BUILDSTORM_COMPILE ... ok=true`、测试组 `END` 或 `shutdown!`，仍不是端到端性能样本。

首个完整快照排除了挂载 I/O，并验证 C/Rust 请求口径一致：

```text
get_ops=5803 = cache_hits 5599 + cache_misses 204
loader_ops=204 = loader_successes 204 + loader_errors 0
writeback_ops=395 = writeback_successes 395 + writeback_errors 0
initial 16 + allocations 516 - drops 517 = resident 15
C I/O: read 983/983, write 461/461
Rust Disk: submits 1444 = read 983 + write 461 = completed 1444
```

末个完整累计快照仍闭合：`get=661695=412142+249553`，loader 为 `249553/249553/0`，writeback 为
`18515/18515/0`，`16+266818-266818=16`，C read/write submit 与 completion 分别为 `279299/279299`、
`30018/30018`，Rust 为 `309321=279303+30018=completed`。报告线程和 I/O 可并行执行，个别 interval 中出现
start/end 暂差 1 或请求类型在相邻快照间移动，后续完整快照恢复，不是丢请求。

容量反馈则明显不收敛：CMake 的 cache 目标只有 16 blocks，但累计 `shake_full_dirty=401664`、
`capacity_overflows=181972`，`max_resident_blocks=1243`。同时 `allocation_races=0`、loader/writeback wait 为 0、
`Disk::dev contended=0`，符合旧 `EXT4_OP_LOCK` 仍串行所有 C 调用的预期。这证明 P21.2b 应先解决 dirty-only
压力，当前没有数据支持优先拆设备锁或实现 waiter 分桶。

## P21.2b 首次实验与回退

维护者提供的 `tmp_11.ans` 运行到 `BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok` 后的 untimed
`pre-build tg-xtask`。最后一个累计快照位于 `t=74910ms`：`resident_blocks=15`、`max_resident_blocks=82`，新增
reclaim 计数仍为零；紧随其后的高 churn workload 才达到 256-block high watermark，因此这条快照不能用于判断 fallback
是否执行。

首次 P21.2b 实现尝试在 high watermark 下，持 `index_lock` 从 `lru_root` 选择 `refctr==0` 的 dirty victim，直接
`RB_REMOVE`，为其加 pin 后锁外调用 `ext4_block_flush_buf()`，最后回锁 drop 或重新插入。该路径在新的日志中触发：

```text
---- KERNEL PANIC IN S-MODE ----
Cause: Exception(LoadPageFault)
stval: 0x70
sepc : 0xffffffc080369392
```

对包含该代码的 RISC-V ELF 运行 `riscv64-linux-gnu-addr2line -f -C`，`sepc` 定位到
`ext4_buf_lru_RB_REMOVE_COLOR` 的 `ext4_bcache.c:67`；反汇编调用点位于新 reclaim victim 的 `RB_REMOVE`。这不是
普通的性能样本，而是 intrusive RB tree 生命周期或所有权假设不成立导致的内核错误。即使 victim 从 LRU tree 而非
dirty list 遍历得到，直接拆树仍会故障，因而不能把 `refctr==0` 与一次遍历结果视为可安全转移 index ownership 的证明。

已撤销该实验的 dirty slack、锁外 writeback 和树/list 重插逻辑，恢复 P21.2a 的 clean-only shake 行为；不保留可能在
默认 `lwext4-smp` 路径触发 panic 的 feature。由于回退后没有保留内核源码改动，本次不以这份中断样本报告性能结果。

## 下一步优化方案

1. **P21.2b.0 生命周期 oracle。** 先以 C 层可重复 trace 覆盖 acquire/release、load、flush、clean eviction 和
   dirty-list 转换；在每一步检查 `refctr`、LBA/LRU tree membership、dirty-list membership 与 `ref_blocks` 守恒。
   完成同 LBA single-flight、不同 LBA、dirty/flush/evict、读写错误和 10 万次随机生命周期后，才允许容量代码接触
   intrusive index。
2. **P21.2b.1 单一 ownership API。** 不让容量策略自行 `RB_REMOVE`/`RB_INSERT` 或调用内部 drop helper；先定义由
   bcache 生命周期模块实现并由 oracle 覆盖的 claim/release API。它必须在统一状态机中完成 victim pin、index 转移、
   writeback 完成和失败恢复，且调用者不能接触 LRU/dirty list 链接。
3. **默认关闭的容量实验。** API 及 oracle 通过后，才以 256/128 blocks 高/低水位做单变量实验，并验证
   `resident <= high + in_flight_loaders`、I/O error 恢复、RISC-V 8 HART QEMU 和 fsck。shared admission/read gate
   在此之前仍不得撤退。
4. **暂缓无证据改动。** loader/writeback wait 和设备 contended 均为 0，因此不同时拆 `Disk::dev` mutex 或做 LBA
   waiter 分桶；shared read 真正产生 wait/wake 数据后再决定分桶数，避免把容量实验与第二变量混在一起。
5. **P21.3 只读并发。** 先建立稳定 inode number/generation 对应的 `Ext4InodeState` 和独立 open-file `f_pos`，
   只让不同 inode 的 `read/read_all/fstat/read_dir` 获取 shared admission；修改操作和 dirty-capacity fallback 继续走
   exclusive gate。验收必须看到 `max_active_readers > 1`，并使 `read/find/fstat` gate wait 在相同 Cargo checkpoint
   明显下降。
6. **性能验收。** 固定镜像、冷启动、QEMU `-smp`、Cargo jobs 和 crate checkpoint 做两次 A/B；比较 16-block
   无界 overflow 与 256/128 有界退让的 full-dirty、overflow、回写批次、gate hold 和 Cargo checkpoint。最终以两次完整
   `BUILDSTORM_COMPILE ... ok=true`、测试组 `END`、`shutdown!` 和无 fsck 差异为准，不用本次 `21/446` 截断样本
   宣称加速或回退比例。

## 涉及文件

- `crates/lwext4_rust/c/lwext4/include/ext4_bcache.h`
- `crates/lwext4_rust/c/lwext4/src/ext4_bcache.c`
- `crates/lwext4_rust/c/lwext4/src/ext4_blockdev.c`
- `crates/lwext4_rust/src/perf.rs`
- `os/src/drivers/disk.rs`
- `os/src/fs/ext4_lw/sb.rs`
- `os/src/utils/perf/fs.rs`
- `os/src/utils/perf/report.rs`

## 验证

已通过：

```text
make musl-generic ARCH=riscv64 LWEXT4_BUILD_DIR=build_musl-generic-riscv64-p212a
make musl-generic ARCH=loongarch64 LWEXT4_BUILD_DIR=build_musl-generic-loongarch64-p212a
make build-arch TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
git diff --check
```

双架构 release/perf 构建与 lwext4 C 构建通过，只有仓库已有的 `smoltcp`、`ext4_fs.c`/`ulibc.c` warning。
维护者的 RISC-V 8 HART `log.ans` 验证上述 telemetry 和守恒，未出现 panic、lwext4 assert、TFAIL 或 TBROK；样本
止于 Cargo `21/446`。P21.2 并发 oracle、I/O 错误注入、fsck 和完整 BuildStorm 尚待执行。
