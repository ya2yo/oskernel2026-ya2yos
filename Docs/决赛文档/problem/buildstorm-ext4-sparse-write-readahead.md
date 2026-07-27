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

- 最多保存 16 个 range、总字节最多 64 KiB；相邻写会合并，交错/重叠写保留写入顺序，读取时按
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

这三条为当前最新一次 `log.ans` 分析后加入，尚未出现在该旧样本中。它们只在 perf 构建下读取
tick，并以 scope guard 覆盖错误返回；下一次同入口运行可据此决定应优化 descriptor 打开、配额
预留还是实际写入/稀疏缓存处理，不能再仅凭 `ext4_write_lock` 总量猜测。

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
- 未在最后一次新增 write phase 统计后启动 QEMU 长测，以免干扰维护者的 `disk.img`、运行环境和
  `log.ans`。因此新 `ext4_write_duration` 字段只完成编译验证，尚无 guest 样本。

## 后续

下一次应使用相同 RISC-V 镜像、内存、hart 数和 BuildStorm 入口，至少运行到与当前相近的
`Building 9/446`，同时记录 Cargo 阶段、`ext4_read_lock`/`write_lock` 的 samples/wait/hold、
`readahead_ops/pages`、sparse 计数以及三个 `ext4_write_duration` 桶。只有当同阶段的重复样本
确认某个写阶段占主导后，才应缩小对应临界区；不应再次扩大预读窗口或放宽 lwext4 全局锁。
