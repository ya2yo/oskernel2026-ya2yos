# BuildStorm EXT4 稀疏写缓冲、顺序预读与锁统计收敛

## 背景

RISC-V final-2026 的 `buildstorm::compile::run()` 会并发启动 Cargo/Rustc，持续产生临时
artifact，并通过 mmap、普通读和小块显式 offset 写入访问同一 EXT4 挂载。lwext4 的路径式 API
和块缓存当前不是 SMP-safe，因此 Ya2yOS 必须继续用唯一 `EXT4_OP_LOCK` 串行进入第三方库；优化
目标是减少不必要的锁进入和缩短临界区，而不是放宽这条安全边界。

本复盘覆盖当前工作区中围绕 `5.ans`、`6.ans`、`7.ans`、`8.ans`、`9.ans`、`10.ans`、
`11.ans` 及后续多次 `log.ans` 进行的连续优化。测试入口使用维护者工作区中直接调用
`buildstorm::compile::run()` 的配置；该 `user/src/bin/initproc.rs` 变更属于维护者测试入口，
不属于本文实现的内核优化。

## 现象

此前已消除 Rustc rename 中的全挂载 flush 尖峰后，BuildStorm 的主要等待重新集中到普通读、
路径 metadata 和写路径的全局 EXT4 锁。最新两页预读样本在 `t=295402ms` 时显示 Cargo
`Building 9/446`，仍没有 `TPASS`、`TFAIL`、`TBROK`、panic、`ERROR`、`shutdown!` 或最终
`Summary`，因此只能作为中途性能快照，不能声称完整 BuildStorm 已通过或给出端到端加速比例。

该快照的主要计数为：

| 项目 | 当前两页预读样本 |
| --- | ---: |
| `ext4_read_lock` samples / wait / hold | 29,453 / 424.939015 s / 53.578775 s |
| `ext4_write_lock` samples / wait / hold | 3,183 / 222.935149 s / 56.992613 s |
| `ext4_find_lock` samples / wait / hold | 10,432 / 99.214718 s / 20.592327 s |
| `ext4_fstat_lock` samples / wait / hold | 4,633 / 59.094352 s / 20.426680 s |
| `readahead_ops / pages / bytes` | 6,962 / 6,962 / 28,485,589 |
| sparse buffer / flush | 2,879 ops、8,169,828 B / 308 ops、8,123,665 B |
| sparse read overlay | 1,681 ops、6,726,537 B，覆盖脏字节 2,107,237 B |

`wait_us`/`hold_us` 是所有 hart 的累计量，不能与约 295 秒 guest wall-clock 直接相加；它们用于
对齐同一 Cargo 阶段下的锁争用趋势。

## 分析

### 稀疏 inode 的小写入被过早提交

whole-file write-back cache 只保存字节，不能表达洞的 extent 布局。稀疏写一旦走该 cache，回写
零填充字节就会把洞物化；因此已有策略会禁用 whole-file cache 并让每个小写入直接进入 lwext4。
Rustc/链接器恰好常对同一稀疏 inode 连续或交错写入小范围，直接写会反复取得唯一 EXT4 锁。

旧 `file_open_inner()` 在同一路径从 `O_RDWR` 切到 `O_RDONLY` 时也会立即 flush sparse 脏数据。
这样后续读只能看到已经落盘的数据，无法使用内存中的脏 range；为了正确性付出的代价是大量小
`ext4_fwrite()` 和更长的写锁队列。

### 页缓存预读的窗口不能盲目扩大

连续页读取原先会对每个 page miss 单独调用 `inode.read_at()`，即每页都进入一次 lwext4 锁。
三页总读取（当前页加后两页）虽然降低了 read 锁请求数，但一次 12 KiB lwext4 读取会明显增加
临界区长度。在与当前样本接近的 `Building 9/446` 历史窗口中，其结果反而更差：

| 指标 | 三页预读 | 当前两页预读 |
| --- | ---: | ---: |
| 时间点 | 297.264 s | 295.402 s |
| `ext4_read_lock samples` | 28,768 | 29,453 |
| `ext4_read_lock wait` | 525.574286 s | 424.939015 s |
| `ext4_read_lock hold` | 68.483890 s | 53.578775 s |

两个未完成运行不是严格 A/B，且总读取字节不同，不能把差值报告为确定的加速百分比；但三页方案
在较少 samples 下同时得到更高 wait/hold，足以否定“继续扩大预读窗口”的假设。

### 重复 metadata 查询仍会放大锁竞争

一次 `Ext4Inode::find()` 已通过 `inode_type_and_stat_at()` 得到 `(st_dev, st_ino)` 及普通文件
大小，但旧路径随后会为 `FsIndex` 的 cache key 再做一次 `fstat()`。此外，末级的
`O_NOFOLLOW`/内部 `O_UNLINK` 查询为防止缓存最终 symlink 而完全绕过 inode cache，连已经确认的
regular file 和 directory 也无法命中。它们都让纯路径/metadata 工作重新排队到 `EXT4_OP_LOCK`。

## 根因

当前吞吐瓶颈并非单个“慢 syscall”，而是以下可证明的串行化来源叠加：

1. 稀疏 inode 缺少保留洞布局的有界脏写缓冲，导致连续小范围写入频繁进入 lwext4。
2. 同一 sparse inode 的读描述符切换会把可在内存中可见的脏 range 过早 flush。
3. 页缓存按页冷读会重复进入 EXT4；反之把读取批量扩大到三页又使一次锁持有过长。
4. 首次路径查找结果没有完全复用，VFS cache 也缺少对保留最终 symlink 标志的精确区分。

lwext4 的全局串行仍是正确性要求：本轮没有把第三方文件系统改成无锁或跨 hart 并发访问。

## 修复

### 有界 sparse range 写缓冲与读时覆盖

`crates/lwext4_rust/src/file.rs` 为禁用 dense whole-file cache 的 regular inode 新增按
`(mountpoint, inode)` 索引的 sparse range 集合：

- 最多保存 32 个 range、总字节最多 64 KiB；相邻写会合并，交错/重叠写保留写入顺序，读取时按
  顺序重放，满足 pwrite 风格的 last-write-wins。
- 每个 range 保持原始 offset；最终 `ext4_fwrite()` 只写用户提供的字节，绝不为洞合成零填充。
- 同一路径的 `O_RDWR -> O_RDONLY` descriptor 切换不再立即 flush。读取先从磁盘读取现有 extent，
  将逻辑 EOF 外的可见洞补零，再覆盖脏 range，因此另一个 open file description 可立即观察到
  已写数据。
- close、sync、rename、truncate、`fstat`、`SEEK_DATA`、`SEEK_HOLE`、inode 最后删除和更换为另一
  pathname 等需要持久性或观察分配布局的边界仍会先 flush。这样 `st_blocks`、extent seek、rename
  可见性和错误重试不会由纯内存 buffer 伪造。
- `file_size()` 以磁盘 EOF 与脏 range 最大末尾的较大者作为逻辑大小，避免 `SEEK_END` 或后续写入
  使用过期 EOF。

当前日志中 2,879 次 sparse buffer 操作只形成 308 次 flush，且 1,681 次读取实际使用了脏 range
覆盖；这证明同路径 descriptor 切换不再破坏读时覆盖路径。

### 路径与 inode identity 复用

- `Ext4Inode::find()` 将已取得的 `ext4_inode_stat` 交给新 inode，保留不可变的
  `(st_dev, st_ino)`；`FsIndex` 优先通过 `Inode::cache_identity()` 建立 key，无法取得时才回退
  `fstat()`。inode reuse 保护仍使用 live `fstat()`，不会因缓存 identity 接受已 unlink 后被复用的
  inode number。
- `open_inner()` 对 `O_NOFOLLOW`/`O_UNLINK` 保留最终 symlink 的 Linux 语义；只有实际是 symlink
  才排除常规 pathname cache，regular file 和 directory 可以安全复用缓存。
- dentry、FsIndex、cached-parent/root lookup 只增加聚合命中/失效统计，缓存淘汰和 alias 生命周期的
  原有边界不变。

### 受限的两页顺序预读与 byte-cache 快路径

- 当前页 miss 且前一页已经缓存、文件尚有下一页时，`FilePageCache` 一次读取当前页和下一页
  （8 KiB），当前页返回、下一页仅在有有效数据且尚未被并发加载时插入缓存。
- 随机读取、第一页、尾页、页帧分配失败及已有下一页的竞争情形保持单页行为。
- 对 dense delayed byte cache，`Ext4Inode::read_at()` 在取得 `EXT4_OP_LOCK` 前先查缓存；命中只做
  内存复制和动态链接兼容补丁，不再为已在内存的数据排队等待无关 block I/O。
- 三页预读试验已回退，正式实现只保留当前页和一页预读；`readahead_pages == readahead_ops` 是这条
  不变量的运行期检查。

### perf 统计

`os` 的 `perf` feature 现在透传到 `lwext4_rust/perf`。第三方 wrapper 自身用 relaxed atomic
累积 whole-file cache、direct write、sparse buffer/flush/read-overlay 的次数和字节数，内核每次
既有定期 `[perf]` 报告读取快照；普通 release 构建不启用这些 atomics。

`os/src/utils/perf.rs` 还增加 VFS cache、预读、byte-cache read hit 统计，并将
`Ext4Inode::write_at()` 的锁内活动分成互斥的 `open`、`quota`、`data` 三段：

```text
[perf] ext4_write_duration open(samples=... total_us=... max_us=...)
[perf] ext4_write_duration quota(samples=... total_us=... max_us=...)
[perf] ext4_write_duration data(samples=... total_us=... max_us=...)
```

这三条只在 perf 构建下读取 tick，并以 scope guard 覆盖错误返回。后续 `log.ans` 已验证它们能将
写锁内的 descriptor 打开、配额预留和实际写入分开；仍需按同一 Cargo 阶段比较，不能再仅凭
`ext4_write_lock` 总量猜测。

## 涉及文件

| 文件 | 修改 |
| --- | --- |
| `crates/lwext4_rust/Cargo.toml` | 新增可选 `perf` feature。 |
| `crates/lwext4_rust/src/file.rs` | sparse range 缓冲、读时覆盖、真实布局边界 flush 与 write-back cache 聚合统计。 |
| `os/Cargo.toml` | 将内核 `perf` feature 透传至 lwext4 wrapper。 |
| `os/src/fs/ext4_lw/inode.rs` | identity 复用、byte-cache read 快路径和写锁内三阶段计时。 |
| `os/src/fs/vfs.rs` | 增加可选 inode identity 接口。 |
| `os/src/fs/kernel_fs_ops/fsidx.rs` | 复用 identity 建 key，并记录 cache/reclaim 统计。 |
| `os/src/fs/kernel_fs_ops/open.rs` | 精确处理保留最终 symlink 的缓存边界，记录路径 lookup 统计。 |
| `os/src/fs/dcache.rs` | 记录 dentry hit/miss/capacity eviction 统计。 |
| `os/src/fs/page_cache.rs` | 保留两页总读取的顺序预读。 |
| `os/src/utils/perf.rs` | 输出 VFS、预读、byte-cache、write-back cache 及写路径分段统计。 |

## 验证

已在最终统计代码加入后执行：

```text
cargo fmt --manifest-path os/Cargo.toml
git diff --check
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
make TARGET_ARCH=riscv64
```

- RISC-V64 与 LoongArch64 的 perf 构建均通过。
- 常规 `make TARGET_ARCH=riscv64` 通过，并完成仓库默认的 RISC-V64、LoongArch64 两套 release
  构建。
- `git diff --check` 通过；构建仅有 vendored `smoltcp` 既有的两个 unused-import warning 和一个
  dead-code warning。
- 后续维护者提供的 RISC-V `log.ans` 已含 write phase 的 guest 样本，结果见下节；本代理没有以
  `make run` 删除或改写维护者保留的 `disk.img`。

## 后续

下一次应使用相同 RISC-V 镜像、内存、hart 数和 BuildStorm 入口，至少运行到与当前相近的
`Building 15/446`，同时记录 Cargo 阶段、`ext4_read_lock`/`write_lock` 的 samples/wait/hold、
`readahead_ops/pages`、sparse 计数以及三个 `ext4_write_duration` 桶。只有当同阶段的重复样本
确认某个写阶段占主导后，才应缩小对应临界区；不应再次扩大预读窗口或放宽 lwext4 全局锁。sparse
计数必须先补齐“每次 flush batch”和“触发原因”口径，不能仅由 `sparse_flush_ops` 推断 buffer 被提前
提交的次数。

## 2026-07-28：读写描述符复用与 write phase 样本验证

新的 RISC-V `log.ans` 使用相同的 8-hart BuildStorm `compile` 入口，在 `t=257349ms` 已显示
`Building 15/446`，未见 `panic`、`ERROR`、`TFAIL`、`TBROK`、`shutdown!` 或最终
`BUILDSTORM_DEBUG_COMPILE`。它仍是外层运行结束前的中途快照，不能用作完整 BuildStorm 通过或
严格端到端 A/B 结果。

此前的样本在 `t=290321ms` 附近仍为 `Building 10/446`，其中写路径的 `open` 阶段为
`38080447 us / 3912 samples`，约 `9.74 ms`/sample。审计 `Ext4File::file_open_read_only()` 发现：
同一 canonical `Ext4Inode` 已持有 `O_RDWR` descriptor 时，读操作仍重新以 `O_RDONLY` 打开；紧随的
写操作再切回 `O_RDWR`。这两个模式都允许读取，并且描述符位置和 lwext4 调用始终由
`EXT4_OP_LOCK` 串行，因此该切换没有提供额外的隔离或 Linux 可见语义。

`file_open_read_only()` 现对同路径、已打开的 `O_RDONLY` 或 `O_RDWR` descriptor 直接返回。它不绕过
全局锁，不改变 sparse range 读时覆盖、write-back、close、sync、rename 或错误重试边界；只删除了
读写交错时多余的 pathname lookup 和 descriptor 建立。

新样本在 `t=257349ms` 的 `open` 阶段为 `5526703 us / 5711 samples`，约 `0.97 ms`/sample；即使样本
阶段和输入工作量不同，较低的每样本开销也与该定向改动一致。同期 `ext4_write_lock` 为
`204.334609 s` wait、`35.072056 s` hold，write phase 分别为 open `5.526703 s`、quota `5.056378 s`、
data `24.040277 s`。这把后续重点明确收敛到实际写入，而不是继续优化 descriptor 打开。

`sparse_flush_ops=689`、`sparse_flush_bytes=16578252` 中的 `ops` 实际按每个底层
`ext4_fwrite()` range 递增，并不表示 `flush_sparse_write_buffer()` 调用了 689 次；约 24 KiB 只是已
提交 range 的平均长度，不能据此判断 16-run/64 KiB 阈值或 close、rename、fstat 等可见性边界到底触发
了多少 batch。故当时未盲目扩大 sparse buffer，避免增加内存上界却不能确定减少真实 lwext4 I/O。
`make perf TARGET_ARCH=riscv64`、
`make perf TARGET_ARCH=loongarch64`、格式检查和 `git diff --check` 均通过；构建只含既有 smoltcp
warning。

## 2026-07-28：32-run sparse 样本复核与下一步计划

维护者提供的新 `log.ans` 是约 5 分钟的 RISC-V 运行结果。最后一个 guest perf 快照为
`t=216291ms`，Cargo 到 `Building 8/446`；日志未包含 `TPASS`、`TFAIL`、`TBROK`、panic、`ERROR`、
`shutdown!`、`BUILDSTORM_DEBUG_COMPILE` 或最终 `Summary`，所以它是未完成的中途快照。

### 32-run 的实际证据边界

新旧样本的 sparse 数据量几乎相同，但 Cargo 阶段不同：

| 指标 | 旧 16-run 样本 | 新 32-run 样本 | 变化 |
| --- | ---: | ---: | ---: |
| `sparse_buffer_bytes` | 16,620,418 B | 16,607,608 B | -0.08% |
| `sparse_flush_ops` | 689 | 628 | -8.85% |
| `sparse_flush_bytes` | 16,578,252 B | 16,560,271 B | -0.11% |
| 每个已提交 range 的平均字节数 | 24,061 B (23.50 KiB) | 26,370 B (25.75 KiB) | +9.59% |

这里的 `sparse_flush_ops` 在 `flush_sparse_write_buffer()` 的 `while` 循环内、每次成功的
`ext4_fwrite()` 后递增；它是已提交的 range 数，**不是** sparse buffer flush batch 数。因而表格仅
说明在相近 payload 下落盘 range 更少、平均 range 更长，可能与相邻写合并或不同 Cargo 工作负载有关；
它不能证明 16->32 减少了 range-limit 触发的提交，也不能给出端到端加速比例。

当前实现仍将 `MAX_SPARSE_WRITE_BUFFER_RUNS` 保持为 32，而 `MAX_SPARSE_WRITE_BUFFER_SIZE` 保持为
64 KiB。这个尝试没有改变洞布局、写入顺序、last-write-wins、读时覆盖和各类可见性 flush 边界；
但在拿到正确 batch 统计之前，不应继续提高到 64 个 range 或扩大 byte 上限。

### 当前主瓶颈

最终快照的 EXT4 全局锁统计如下。wait/hold 为所有 hart 的累计时间，不能与 216 秒 guest wall-clock
相加，但类别间的高 wait/hold 比例直接表明并发任务主要在等待唯一的 lwext4 入口：

| 锁类别 | samples | wait | hold |
| --- | ---: | ---: | ---: |
| `ext4_read_lock` | 31,942 | 241.236 s | 24.419 s |
| `ext4_write_lock` | 5,609 | 140.405 s | 24.455 s |
| `ext4_find_lock` | 10,511 | 80.670 s | 21.899 s |
| `ext4_fstat_lock` | 4,666 | 30.881 s | 15.607 s |
| `ext4_rename_lock` | 18 | 3.755 s | 3.609 s |
| 合计 | 52,746 | 496.947 s | 89.987 s |

write 锁内 `data` 为 `16.519 s / 5609` 次，高于 open `3.923 s` 和 quota `3.780 s`；不过它只解释写锁
持有的一部分，主要损失仍是 read/write/find/fstat 的排队。页缓存已有 `515047` hit、`24559` miss 和
`7110` 次两页预读，不能仅凭这些累计数把预读窗口再扩大到三页，历史试验已显示那会增加锁持有。

此外 `lseek` 有 39,061 次 type-check，累计 `3.400 s`，其中几乎全部来自每次调用先构造 pathname、查询
`FsIndex::special_node_type()` 再回退 inode 固定类型；这是独立于 EXT4 锁的高频小路径，但量级低于全局
锁排队，不应抢在读路径前进行未经验证的大改。

### 下一步优化计划

1. **先补齐 sparse 计数口径，不改容量策略。** 在 `lwext4_rust/perf` 用 relaxed atomic 记录非空
   `flush_sparse_write_buffer()` 的 batch 数、每 batch 的 range 数/字节数/最大值，并按 `range_limit`、
   `payload_limit`、分配失败、大写直通和可见性边界分别累计原因。保持定期聚合输出，不增加逐调用
   日志。下一轮只在 `range_limit` batch 占比可观且 32-run 仍频繁触发时，才讨论进一步调整。
2. **定位读锁的可消除进入点。** 将 `FilePageCache` 的 hit/miss 分为 mmap fault、普通 `read()`、
   `splice` 三个来源，并统计 `OSFile::try_page_cached_read()` 因文件超过 8 MiB、请求超过 64 KiB 或非
   regular inode 而旁路的次数和字节数。这样可判断 24,559 次 miss 是否主要来自可安全复用的 toolchain
   输入，而不是盲目扩页缓存。
3. **按统计结果实施受限缓存。** 只有当重复读取主要落在 8 MiB 阈值外的 immutable regular file 时，
   才为该类文件增加有内存上限、可失效的页缓存准入；当前 `FilePageCache` 不设容量淘汰，不能直接提高
   8 MiB 阈值。仍保持两页预读、写入/截断/rename 失效和 mmap COW 语义。
4. **最后处理 lseek 小路径。** 若补充的命中统计确认 `special_node_type()` 对 BuildStorm 的 39,061 次
   检查几乎全为 regular-file miss，可在 `OSFile` 创建时缓存不可变的 seekability/type，避免每次
   `lseek` 复制路径并查询全局 special-node 表；需保留 FIFO/socket 返回 `ESPIPE` 的语义，并以定向
   `lseek`/FIFO 回归验证。

## 2026-07-28：`tmp_10`/`tmp_11` 复核与页缓存来源统计

### 样本结论

维护者指出应以 `tmp_10.ans` 而不是较早的 `tmp_09.ans` 对照 `tmp_11.ans`。两份日志都使用
`buildstorm::compile::run()` 入口，但都停在 Cargo `Building N/446` 阶段，没有 `TPASS`、`TFAIL`、
`TBROK`、`Summary` 或 `shutdown!`；`syscalls total` 也包含 futex 与未分类 syscall，不能充当编译吞吐。

在约 70 秒快照中，两者的 `open`、`stat`、`read`、`write` 数量近似，而 `tmp_11` 的 EXT4 分类锁等待
合计约为 `1.052 s`，低于 `tmp_10` 的约 `2.339 s`。约 100 秒时，`tmp_11` 的对应等待合计约 `53.715 s`，
仍低于 `tmp_10` 的约 `69.871 s`。但 Cargo 并发任务与 crate 顺序不同，且运行均未完成，因此这些数据既
不能证明端到端回归，也不能报告加速比例。

### 不扩大预读窗口

本轮曾按 Linux readahead 的批量 I/O 思路评估将 `FilePageCache::get_or_load()` 扩展为四页冷页窗口。
在提交前复核本问题的既有证据后撤回：同一文档已经记录三页试验于 `t=297.264 s` 的
`ext4_read_lock wait/hold=525.574286/68.483890 s`，而正式两页方案在相近 `t=295.402 s` 为
`424.939015/53.578775 s`。样本非严格 A/B，不能报告百分比，但在更少 read 样本下同时出现更高 wait/hold，
足以否定继续扩大预读深度的假设。

当前正式行为保持不变：顺序信号仅为“前一页已缓存、当前页缺失”，一次读取当前页与一页预读页；已缓存页、
尾页、分配失败及随机访问仍回退原行为。没有重新引入曾发生停滞的 page-loading waiter。

### 修改

为确定下一轮可安全合并的冷读来源，新增低开销的 `perf` 聚合统计，不改变缓存准入、页内容、失效、锁或
读取语义：

- `FilePageCache::get_or_load()` 的既有 hit/miss 按 `mmap`、普通 `read`、`splice` 归因；普通多页
  `read()` 还按实际页探测累计 `read_page_hit/read_page_miss`，因此这两个字段不与历史总计数直接相加。
- `OSFile::try_page_cached_read()` 记录普通 read 因请求超过 64 KiB、文件超过 8 MiB 或非 regular inode
  旁路页缓存的操作次数与请求字节数。
- 定期 perf 输出增加独立的 `file_cache_source` 行；旧 `ext4 ... file_cache hit/miss` 输出不变，历史
  解析脚本和日志对比不受字段迁移影响。

涉及 `os/src/fs/page_cache.rs`、`os/src/fs/files/os_file.rs`、`os/src/mm/memory_set/handle.rs`、
`os/src/syscall/io_mpx/splice.rs` 与 `os/src/utils/perf.rs`。维护者已有的 `user/src/bin/initproc.rs`
和未跟踪 `disk.img` 未触碰。

### 验证与后续样本

已执行 `cargo fmt --manifest-path os/Cargo.toml --check` 与 `git diff --check`。维护者已明确要求不再构建，
故未执行 `make`、QEMU、BuildStorm 或双架构编译；尚无包含新统计的 guest 样本。

下一次同配置运行应同时读取：

```text
[perf] ext4 ... file_cache hit=... miss=...
[perf] file_cache_source mmap_hit=... mmap_miss=... read_page_hit=... read_page_miss=...
       splice_hit=... splice_miss=... read_bypass_*_ops=... read_bypass_*_bytes=...
```

只有当旁路大头可归因到可失效、受内存上限约束的 immutable regular file，才评估扩大该类文件的缓存准入；
若冷页主要来自 mmap fault 或 splice，则应分别合并相邻缺页而不是扩大全局预读窗口。

## 2026-07-28：`tmp_13` EXT4 锁分类与页缓存重复加载复核

### 背景

维护者根据 `tmp_13.ans` 要求继续按 EXT4 各类锁的优先级推进。本轮先验证两个可证伪假设：

1. 除原有 `read/find/fstat/write/rename` 外，`close` 或通用 metadata/namespace 路径可能才是当前全局锁排队的主要来源。
2. `FilePageCache::get_or_load()` 的同页重复底层读取足够频繁，值得用 single-flight/waiter 合并加载。

lwext4 的挂载级块缓存和路径式 API 仍不具备 SMP 安全保证，因此统计过程不能拆除
`EXT4_OP_LOCK`，也不能让多个 hart 并发进入 lwext4。

### 修改

`Ext4OpLock` 增加分类 profile guard，仍然只获取同一把 `TaskMutex`。除既有读、查找、fstat、写入和 rename 外，新增
`close`、`read_all`、`read_dir`、`path_resolve`、`metadata`、`namespace`、`sync`、`seek` 八类；分别覆盖底层
descriptor close、完整/目录读取、软链接与 live-path 恢复、属性访问、创建/删除/链接、显式刷新和 hole/data seek。
每类计数保留 samples、累计 wait/hold tick 和 wait/hold 最大值。

profile guard 在释放 `TaskMutexGuard`、唤醒一个等待者以后才更新 relaxed atomic，避免统计本身延长 lwext4 临界区。
页缓存同时增加两个纯统计字段：`load_attempts` 在 miss 后即将进行底层读取时递增，`load_races` 在 I/O 返回后发现另一
hart 已发布同一页时递增。它们不改变缓存锁、I/O、失效、COW 或预读行为。

涉及文件：`os/src/fs/ext4_lw/mod.rs`、`os/src/fs/ext4_lw/inode.rs`、`os/src/fs/ext4_lw/sb.rs`、
`os/src/fs/page_cache.rs`、`os/src/utils/perf.rs`。维护者已有的 `user/src/bin/initproc.rs` 和未跟踪 `disk.img`
未触碰。

### RISC-V 运行期样本

两次均以相同 BuildStorm `compile` 入口运行，宿主外层在 130 秒终止：

```text
/usr/bin/time -f 'elapsed_s=%e user_s=%U sys_s=%S' \
  timeout 130s make run TARGET_ARCH=riscv64 > /tmp/<sample>.log 2>&1
```

锁分类样本最后快照为 `t=106614ms`，Cargo 约为 `Building 6/446`。取 `t=72518ms -> 106614ms` 的累计差值，
EXT4 全局锁新增 `15522` 次，累计 wait/hold 为 `70.547/17.446 s`：

| 类别 | 新增 samples | 新增 wait | 新增 hold |
| --- | ---: | ---: | ---: |
| `read` | 13,158 | 62.674 s | 7.946 s |
| `find` | 746 | 3.437 s | 2.258 s |
| `fstat` | 465 | 1.364 s | 1.449 s |
| `write` | 117 | 1.563 s | 1.348 s |
| `close` | 92 | 0.000114 s | 0.008833 s |
| `metadata` | 403 | 0.873 s | 2.221 s |
| `namespace` | 153 | 0.483 s | 1.727 s |

`read` 独占该窗口约 `88.8%` 的全局锁等待，仍是首要方向；`close` 不是当前的通用热点。`metadata` 和
`namespace` 的最大持锁分别约为 96 ms、97 ms，但当前类别仍包含多个函数，不足以安全缩小任一临界区。

该样本出现 Rustc `SIGSEGV`，但项目已有多个历史 BuildStorm 样本出现相同偶发异常；本轮新增的仅是聚合统计，
不能将该现象归因于此修改，也不将它当作功能或性能通过结果。

第二次样本最后快照为 `t=94153ms`、Cargo 约 `Building 5/446`，未出现该 `SIGSEGV`。最终
`load_attempts=18080`、`load_races=434`，即约 `2.4%` 的潜在加载在发布时发现另一 hart 已先写入缓存。
重复读取真实存在，但比例不足以抵消 per-page single-flight/waiter map 所引入的锁序、等待和历史停滞风险。

两个样本都被外层 timeout 终止，且没有 `TPASS`、`TFAIL`、`TBROK`、最终 `Summary`、`shutdown!` 或
`BUILDSTORM_DEBUG_COMPILE`；因此不报告端到端加速或完整 BuildStorm 通过。

### 验证与后续

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
```

两个 perf 构建均通过；仅有 vendored `smoltcp` 既有的 unused-import/dead-code warning。下一轮应先将
`metadata` 细分为 size、alias、mode/owner、link count 等，并将 `namespace` 细分为 create、truncate、
unlink、hard link、symlink，再以相同 Cargo 阶段决定是否能缩短某个具体临界区。不要仅凭本轮 `2.4%` 的页缓存
race 引入 single-flight，也不要未经新的 inode 内部并发设计而移除 `add_alias_path()` 的全局锁。

## 2026-07-28：`tmp_01`/`tmp_02` 读锁拆分、四页有界预读与一小时目标

### 样本边界和当前结论

本轮复核维护者提供的 `tmp_01.ans`、`tmp_02.ans`。二者都是 BuildStorm `buildstorm-compile` 的中途输出，
只有 Cargo `Building N/446`，没有 `BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=<...>`、`shutdown!`、
最终 `Summary` 或完成标记。因此它们只能用于热点归因，不能报告完整编译时间、端到端加速比例或“评测机一小时内
完成约 446 个 crates”已经达标。

`tmp_01` 最后约为 `Building 5/446`，`ext4_read_lock` 累计 wait/hold 为 `59.854/9.434 s`。`tmp_02` 在
`t=67.340s` 时约为 `Building 4/446`，其主要指标如下：

| 指标 | `tmp_02` 值 | 结论 |
| --- | ---: | --- |
| `ext4_read_lock` | 17,553 samples，`7.604/4.816 s` wait/hold | 全局 lwext4 gate 仍主要被读取排队 |
| `read_open_lock` | 1,820 samples，`0.052/1.538 s` wait/hold | descriptor 准备不再是读路径主要等待来源 |
| `read_data_lock` | 15,733 samples，`7.552/3.278 s` wait/hold | 实际数据读取仍必须通过唯一后端串行边界 |
| `find_lock` | 9,436 samples，`0.511/8.106 s` wait/hold | 路径查找和锁内工作仍不可忽略 |
| `fstat_lock` | 3,931 samples，`0.106/2.302 s` wait/hold | 属性查询仍频繁进入后端 |
| `metadata_lock` | 1,616 samples，`0.230/5.350 s` wait/hold | 聚合类别尚需进一步细分 |
| `file_cache` | 222,518/14,644 hit/miss | mmap 缺页仍是主要冷页来源 |
| `load_races/load_attempts` | 427/14,647（约 2.9%） | 同页重复加载不足以支持高风险 waiter 重构 |

读锁拆分后的 `tmp_02` 中，实际 data-read 累计等待低于 `tmp_01` 未拆分读路径的约 `59.854 s`，但两个样本的
工作量和 Cargo 阶段不同，不能把它解释为整体加速比例。当前可确认的是：无谓的全局锁入口已减少，但 gate 请求数与
锁内数据读取仍然制约吞吐，距离一小时目标仍有显著差距。

### 已实现的收敛与取舍

`Ext4Inode` 现在通过每 inode 的 `io_state` 保护 descriptor、alias 与 delay 状态。锁序固定为写状态路径
`write_state -> io_state -> EXT4_OP_LOCK`，普通读取为 `io_state -> EXT4_OP_LOCK`。`read_at()` 可据此将
descriptor open 和 data read 分成两个全局锁段，同时保证中间不会被同 inode 的并发操作改变 descriptor；已有同
路径 `O_RDONLY/O_RDWR` descriptor 时也会直接复用。

唯一的 `EXT4_OP_LOCK` 仍然保留。lwext4 的挂载级块缓存和路径式 C API 没有 SMP 并发安全保证，直接改为 rwlock
会破坏后端前提，不能作为性能优化。

页缓存最终采用四页/16 KiB 有界顺序预读：前一页已缓存、当前页缺失且未到 EOF 时，单次最多读取当前页及后三页；
随机访问、文件第一页和 EOF 均保持单页行为。8 页试验扩大了 `read_data` 的单次持锁时间和全局排队，已撤回。
最终四页方案的 120 秒运行中，`readahead_ops=4,658`、`readahead_pages=11,572`，平均约 2.48 页/次；该数据
只证明预读边界的实际行为，不是完整性能 A/B。

### Linux 对照与关键差距

Linux 7.0 的 `ext4_file_read_iter()` 对普通 buffered read 进入 `generic_file_read_iter()`，再由
`mm/filemap.c` 的 `filemap_get_pages()`、同步/异步 readahead 和 `file_ra_state` 调度；ext4 address-space ops
还注册 `.readahead = ext4_readahead`。它使用 per-file mapping、folio 批处理和细粒度页锁管理并发。

Ya2yOS 的页缓存当前仍是全局 `RwLock<BTreeMap<path, BTreeMap<page, FilePage>>>`，并且所有 lwext4 C API
仍经唯一 `EXT4_OP_LOCK`。因此不应照搬 Linux 的大预读窗口，更不能让多 hart 未经证明地同时调用 lwext4。
近期最有效的方向是消除可避免的 `read/find/fstat/metadata` 后端进入；长期才是建设真实的 per-file 缓存和
后端并发架构。

`dentry_positive_hit=0` 是必须优先解释的异常：BuildStorm 含有大量已知 regular file 路径，正向 dentry cache
却没有产生可见命中。这会使 open、路径解析和元数据查询落入 `find`/metadata 的 lwext4 慢路径，比继续盲目扩张
预读更值得优先取证。

### 验证

已执行并通过：

```text
cargo fmt --manifest-path os/Cargo.toml
cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml
cargo fmt --manifest-path os/Cargo.toml -- --check
cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml -- --check
git diff --check
make perf TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
```

RISC-V perf 和 LoongArch64 release 构建均通过，只有 vendored `smoltcp` 的既有 warning。使用 `/tmp` qcow2
overlay 的两次 120 秒 RISC-V QEMU 冒烟均通过 `sigaltstack regression: PASS`、`rseq regression: PASS` 并进入
`buildstorm-compile`，未发现 panic、`TFAIL`、`TBROK` 或 `could not compile`。两次均由外层 timeout 正常结束，
最终四页方案仅可见 `Building 6/446`；没有改写维护者已有的 `disk.img` 或基础 raw 镜像。因此完整 BuildStorm 和
一小时目标仍未验证。

### 一小时目标的后续计划

1. **P0：先获得可复现的完整基线。** 固定 raw 镜像、16 GiB 内存、8 hart、BuildStorm 入口和缓存起始状态，
   用 `/tmp` qcow2 overlay 跑至 guest 输出 `BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=<...>`。只有该行及
   `elapsed_s` 是完成和时限判据，`timeout` 或 `Building N/446` 不是。
2. **P1：恢复正向 dentry 命中。** 从 `open`、`FsIndex` 和 `DENTRY_CACHE` 的插入、失效、lookup 路径查明
   `dentry_positive_hit=0` 的原因；先修复可证实的缓存键、生命周期或准入错误，以消除 `find` 和路径 metadata
   的后端调用。
3. **P2：细分 metadata/namespace。** 分别计量 size、mode/owner、link count、create、truncate、unlink、rename
   和 link，依据完整基线选择可缓存或可合并且语义安全的具体操作，不能由聚合总量猜测性缩锁。
4. **P3：以锁时间证据驱动页缓存改造。** 在保持失效、COW、mmap 语义前提下，评估有容量上限、按 inode/路径分片
   的缓存索引和锁；目标是逐步接近 Linux 的 per-file mapping，而不是无限扩大预读或立即引入有停滞历史的
   per-page waiter。
5. **P4：处理编译后段的写和目录操作。** 根据完整样本的 write/namespace 数据优化 sparse/direct write、
   artifact rename 与 metadata cache，同时保持 fsync、rename、unlink、hole 和崩溃可见性语义。
6. **P5：评估架构上限。** 若上述措施后的完整 benchmark 仍接近或超过一小时，应把 lwext4 的非 SMP-safe 单一
   gate 视为架构限制，评估替换或深度改造文件系统后端；不得未经并发安全证明把 `EXT4_OP_LOCK` 改为 rwlock。

每一项优化都必须用同配置的完整 `elapsed_s` 与基线比较。当前只完成局部读路径与页缓存改动及有限运行期冒烟，
尚不能声称评测机一小时内完成 400 多个 crates。

## `tmp_10.ans` sparse payload 上限归因与有界扩容（2026-07-29）

### 背景

此前 sparse range 缓冲限制为每 inode `64 KiB` payload、32 个 run。`tmp_09.ans` 的 `CacheEvict` batch
平均接近 64 KiB，但旧统计无法区分 payload、run 数、分配失败或大写直通，不能安全直接扩容。

### 现象与分析

维护者提供的十分钟 RISC-V `tmp_10.ans` 最后完整快照是 `t=591581ms`、Cargo `Building 14/446`。
其中 `ext4_sparse_buffer` 的 batch 统计为：

| 指标 | 数值 |
| --- | ---: |
| 全部非空 batch | 439 / 26,239,884 B |
| `CacheEvict` batch | 427 / 26,011,386 B |
| payload limit | 422 / 25,856,958 B |
| run limit | 5 / 154,428 B |
| 两种限制同时命中、分配失败、大写直通 | 0 |

`25,856,958 + 154,428 = 26,011,386`，与原 `CacheEvict` bytes 精确一致；payload 造成
`98.8%` 的容量 batch 与 `99.4%` 的容量字节，32-run 上限并非本轮主要限制。日志只有
`BUILDSTORM_TOOLCHAIN/MINIBUILD ok`，没有完整 compile/end/shutdown，故它只用于局部归因。

### 修复

`lwext4_rust::file` 保持 32 个 sparse run，将单 inode payload 上限从 `64 KiB` 提至 `256 KiB`。为避免
多 inode 同时积累使扩大后的每 inode 上限转化为无界内存，`SPARSE_WRITE_BUFFERS` 改为保存 `total_bytes` 的
wrapper 全局 store，并强制总 payload 不超过 `8 MiB`。

预算命中时，若当前 inode 已有 pending range，沿既有 `CacheEvict` 发布该 inode 的 ranges 后再尝试本次写；
若当前 inode 无 pending range，或发布后全局预算仍不足，则本次写返回原有 direct `ext4_fwrite` 慢路径。不会
为回收预算而强制写回另一个 inode，也不改变 hole 保留、write-order/last-write-wins、读时 overlay，或
`fstat/statx`、rename、truncate、unlink、close、fsync/sync、`SEEK_DATA/SEEK_HOLE` 的可见性边界。

perf 增加 batch runs/bytes/max、payload/run/both/allocation/large-direct/global-budget 原因、预算 direct fallback
以及 `resident_max_bytes`。所有统计仍只在 `perf` feature 下使用 relaxed atomic。

### 验证

已通过 `cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml`、
`cargo fmt --manifest-path os/Cargo.toml`、`git diff --check`、
`make perf TARGET_ARCH=riscv64`、`make perf TARGET_ARCH=loongarch64` 与默认 `make`（RISC-V、LoongArch64
release）。仅出现既有 Cargo config、vendored `smoltcp` 与 release `ipi_sent` warning。

RISC-V 8 GiB/8 hart、final-2026 raw 镜像的 `-snapshot` 120 秒冒烟到达
`BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok` 和 untimed prebuild；无 `panic`、`ERROR`、`TFAIL` 或
`TBROK`，没有改写基础镜像或维护者的 `disk.img`。该窗口没有进入 sparse 写密集的正式编译段，且未运行
`mmap16`、`lseek11`、fstat/statx、rename/unlink 定向回归，完整 BuildStorm `elapsed_s` 和性能提升仍待同配置
长样本验证。

## `tmp_03/tmp_05` rename 写回子阶段归因（2026-07-31）

### 背景

`tmp_03.ans` 末尾的 rename `write_back_cache` 为 `39 / 12.292s`、最大 `2.095s`，但旧统计
无法判断该阶段在提交 sparse ranges、dense whole-file cache，还是只在移除路径表项。
在没有字节来源和子阶段 max 前直接跳过写回，可能使 Rustc 发布的 rmeta/rlib 文件只在旧路径
cache 中可见，或让后续 close/eviction 重建已 rename 的临时路径。

### 取证实现

`lwext4_rust` 在 perf feature 下向 Ya2yOS 同步发送 rename barrier 的六个边界事件，内核以
per-call recorder 将它们聚合为：

- `sparse_write_flush`：提交 inode-keyed sparse ranges；
- `dense_write_back`：检查并提交 path-keyed whole-file cache；
- `path_cache_discard`：移除 FIFO/CACHE_TABLE 表项。

wrapper 另记录每次成功 barrier 的 sparse/dense 实际提交字节、零字节快路径和清理次数。
记账使用 relaxed atomic，没有动 `write_state -> io_state -> EXT4_OP_LOCK` 锁序，也没有改变
“写回旧路径 cache -> close -> 底层 rename -> VFS/cache 失效”顺序。

### `tmp_05.ans` 结果

五分钟 RISC-V 样本的最后完整快照是 `t=273339ms`、Cargo `Building 25/446`，无
panic/TFAIL/TBROK/ERROR/SIGSEGV/rustc error，但无 BUILDSTORM_COMPILE、测试组 END 或 shutdown。
末尾统计为：

| 阶段/计数 | samples/ops | total/bytes | max |
| --- | ---: | ---: | ---: |
| rename write_back_cache | 31 | 1,972,072 us | 378,111 us |
| sparse_write_flush | 32 | 137 us / 0 B | 7 us |
| dense_write_back | 31 | 1,968,769 us / 7,164,409 B | 377,985 us |
| path_cache_discard | 31 | 2,133 us | 423 us |
| zero-byte fast path | 3 | - | - |

`sparse_write_flush` 比成功 rename 多一个 sample，表示外层五分钟终止时有一次 rename 已进入第一个
子阶段、但未完成整个 barrier；字节和 ops 计数只在成功清理后增加，因此仍能与 31 次成功
rename 对齐。dense 阶段占 write_back_cache 累计时间约 `99.8%`，sparse 和纯表项清理可从
主要尖峰候选中排除。

`tmp_03` 在 Cargo 25/446 附近的 write_back_cache 为 `33 / 9.670s`，`tmp_05` 为 `31 / 1.972s`；
同期 create/unlink/mode 也整体快数倍，而 P17 只增加记账、没有行为改动。因此该差异必须解释为
非受控宿主/QEMU/缓存变动，不能报告 P17 加速或外推完整编译时间。

### 结论与后续边界

下一个值得验证的行为候选是：对可证明单链接的 regular file，成功 rename 后把旧路径的
dense cache 在 CACHE_TABLE/FIFO_TABLE 内原子 re-key 到新路径，避免 rename gate 内的整文件写回。
该候选尚未实施；必须先通过 rename 覆盖、立即 read/exec、旧 fd 继写、失败回滚、硬链接、
fsync/sync、unlink-open-close 和 eviction 定向回归。无法证明单一所有权或回归未通过时，保留当前先写回
再 rename 的正确性边界。

### 验证

P17 代码已通过：

```text
cargo fmt --manifest-path os/Cargo.toml --all
git diff --check
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
```

两个架构均编译成功，只有仓库既有 Cargo config 与 `smoltcp` 未使用代码告警。本轮尝试的本地
RISC-V QEMU 采样因受限环境不能在 `/var/tmp` 创建临时文件而未启动；运行期证据来自维护者提供的
`tmp_05.ans`。未运行上述 rename 语义定向回归或完整 BuildStorm，因为本轮只增加 perf 观测，且
五分钟样本没有完整结束标记。

## `tmp_06` 后段吞吐退化与全局 gate 公平性（2026-07-31）

### 现象

本轮 `tmp_06.ans` 运行约 30 分钟，无 panic/TFAIL/TBROK/ERROR/SIGSEGV/rustc error，但没有
`BUILDSTORM_COMPILE`、测试组 END 或 `shutdown!`。Cargo 进度随时间明显变慢：

| guest 时间 | Cargo 进度 | 观察 |
| ---: | ---: | --- |
| 596.509s | 33/446 | 约十分钟检查点 |
| 671.719s | 36/446 | 前段仍可连续推进 |
| 872.646s | 38/446 | 36 -> 37 约 201s |
| 1173.788s | 44/446 | 中段恢复一段进度 |
| 1322.980s | 46/446 | 44 -> 45 约 149s |
| 1579.319s | 54/446 | 最后一次进度刷新 |
| 1750.035s | 54/446 | 至少 170.716s 无新 crate |

因此“五分钟可达 4--5 crates/min”只描述早期小 crate 与当时并发交错，不能外推后半段。30 分钟样本从
33/446 到 54/446 的 1153.526s 只完成 21 项，约 `1.09 crates/min`；这个固定区间指标仍不是完整
BuildStorm 端到端成绩。

### 最后 interval 的两层瓶颈

最后 `170.716s` interval 中，EXT4 操作仍在执行而非完全空闲：

| 路径 | samples | wait | hold/本体增量 |
| --- | ---: | ---: | ---: |
| write-open gate | 136 | 399.685s | 12.727s |
| find gate | 39 | 57.180s | 21.536s |
| fstat gate | 27 | 18.834s | 27.794s |
| namespace gate | 17 | 29.243s | 48.336s |
| read gate | 58 | 91.249s | 11.433s |
| create | 8 | - | 10.667s |
| metadata_apply | 7 | - | 17.993s |
| unlink | 9 | - | 19.665s |
| mode | 9 | - | 18.528s |
| ext4_stat_get | 27 | - | 27.617s |

上述 wait 是多个并发任务的累计值，不能相加为 wall-clock。但 `ext4_write_open_lock` 的全局
`max_wait_us` 从此前 28,964,304 跃升到 175,953,977，一个请求几乎覆盖整个静默窗口；与此同时其他
namespace/find/fstat 请求持续获得锁并完成。这说明第一层问题是 gate 排队长尾，而非单个 176 秒临界区。

源码审计确认 `TaskMutex` 的 sleeper 虽由 `PollSet::wake_one()` 按 FIFO 取出，解锁流程却是：

```text
drop spin::Mutex guard -> wake_one sleeper -> sleeper 等待调度 -> 再次 try_lock
```

释放与被唤醒任务重新执行之间，新调用者仍可通过 `try_lock()` 抢先获得锁，所以 FIFO wake 不等于 FIFO
ownership；高频调用可以放大 barging。聚合日志尚不能给出实际 barging 次数，因此下一轮先增加 queued、
queue depth、handoff 和 barging 计数，再让 mount-wide gate 显式保留给队首。该改动只针对
`EXT4_OP_LOCK`，lwext4 继续单线程串行，不能改成 rwlock。

第二层问题是持锁事务本身也随工作集增大。最后窗口 create/unlink/mode/stat 的平均本体耗时已经达到秒级；
即使公平交接消除饥饿，仍需减少创建后的第二次 metadata transaction，并在 lwext4 C 层继续拆出 path lookup、
directory scan、block allocation、journal 和 block-cache flush，才能解释剩余吞吐退化。

### rename 结论的边界

P17 在长样本中仍成立：到 `t=1579.319s`，93 次 rename dense write-back 提交 `79,225,183 B`，累计
`119.466s`、最大 `25.577s`；到末尾变为 94 次、`79,238,331 B / 122.540s`。最后 171 秒只增加一次
`13,148 B / 3.074s`，所以受限 cache re-key 仍有收益潜力，但不能解释或修复整个末段停滞。

### 下一轮顺序

1. P18.1：为 `EXT4_OP_LOCK` 增加公平性交接计数并实现可取消的 FIFO/ticket handoff，先消除百秒级饥饿。
2. P18.2：把新 inode 的最终 mode/uid/gid 并入创建 transaction，删除第二次 pathname metadata 更新。
3. P18.3：定向语义回归通过后，仅对单链接 regular file 实施 rename dense-cache re-key。
4. P19：若后段单次操作仍退化，拆分 lwext4 C 层 transaction/path/allocation/journal/cache-flush 时间。

页缓存末尾为 `79776/98304` 且 `capacity_bypass_pages=0`，本轮没有证据支持继续扩大缓存。

## `tmp_07` P18.1 公平 gate 验收（2026-07-31）

### 背景与现象

`tmp_07.ans` 是装载 P18.1 公平 `EXT4_OP_LOCK` 后的十分钟 RISC-V 样本。日志最后为
`t=539771ms`、Cargo `Building 24/446`，没有 `BUILDSTORM_COMPILE`、测试组 END 或 `shutdown!`；同时
未出现 `panic`、`TFAIL`、`TBROK`、`ERROR` 或 `SIGSEGV`。因此它只能验收 gate 行为和方向性等待数据，不能给出完整
BuildStorm 的 crates/min 或端到端加速结论。

与 `tmp_06` 的 `ext4_write_open_lock max_wait_us=175953977` 相比，`tmp_07` 的最大等待为：

| 指标 | `tmp_07` |
| --- | ---: |
| `ext4_write_open_lock max_wait_us` | 14,185,616 |
| `ext4_write_lock max_wait_us` | 25,758,208 |
| `ext4_gate_fair max_handoff_wait_us` | 25,757,793 |

最大值已经从跨越整个静默窗口的百秒级等待降至与一次长 lwext4 事务相近的约 25.8 秒。公平交接解决了
“新请求插队导致队首长期得不到所有权”的第一层问题，但没有缩短该长事务本体。

### 统计守恒与实现核对

最后累计快照为：

```text
fast_acquires=38741 queued=32432 handoffs=32426 handoff_wakes=32427
barging_prevented=24401 cancelled=0 queue_depth=6 max_queue_depth=9
handoff_wait_us=2298306230 max_handoff_wait_us=25757793
wait_ge_10s=12
```

队列记账满足：

```text
queued = handoffs + cancelled + queue_depth
32432 = 32426 + 0 + 6
```

等待分桶也闭合：`11554 + 4963 + 11714 + 3874 + 309 + 12 = 32426`。`handoff_wakes` 比已完成
handoffs 多一次，表示样本截断时有 waiter 已被唤醒但尚未完成下一次 poll，不应把它误判为重复所有权。
最后一个 `67962ms` interval 新增 `3293` 次排队、`3292` 次 handoff、队深仍为 6，说明队列持续交接而非永久
阻塞。

代码审计确认：登记、队首判断、释放和 ticket 移除均在 gate 状态锁下完成；释放只唤醒队首，队列非空时
`try_lock` 不会插队；Future Drop 和 `exit_current_and_run_next()` 会删除 stale ticket，队首取消且 gate 空闲
时继续唤醒下一位。该路径保留 `write_state -> io_state -> EXT4_OP_LOCK` 锁序，也未改变 lwext4 的单线程串行
边界。`cancelled=0` 只表示本 workload 没有观测到取消，不替代定向取消/退出测试。

### 修复与涉及文件

- `os/src/fs/ext4_lw/mod.rs`：以 FIFO ticket queue 实现 mount-wide gate，禁止有队列时 fast-path 插队，支持
  Future Drop、队首 handoff 和按 TID 清理。
- `os/src/fs/mod.rs`、`os/src/task/mod.rs`：导出清理入口，并在任务退出前移除 gate waiter。
- `os/src/utils/perf/fs.rs`、`os/src/utils/perf/report.rs`：增加累计及 interval 的队深、交接、插队阻止、取消
  和等待分桶统计。
- `crates/lwext4_rust/src/file.rs`：修复非 perf 构建下写回阶段统计变量的 unused warning。

### 验证边界与后续

本轮已通过 `make perf TARGET_ARCH=riscv64`、`make perf TARGET_ARCH=loongarch64` 和默认
`make TARGET_ARCH=riscv64`（该目标同时覆盖 LoongArch64 release 子目标）；`tmp_07` 无异常但未完成 BuildStorm。
尚需补做无竞争 fast path、64+ waiter、队首取消/任务退出、跨 hart 交接及持锁任务抢占回归，并取得包含
`BUILDSTORM_COMPILE ... ok=true elapsed_s`、测试组 END、`shutdown!` 的完整样本。公平 gate 之后的优先级仍是
创建事务元数据合并、受限 rename dense-cache re-key，以及对 create/unlink/stat 的 lwext4 C 层长事务分段计时。
