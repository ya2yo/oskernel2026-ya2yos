# BuildStorm `lseek` open-file 类型缓存优化

## 背景

`buildstorm::compile::run()` 会让 Cargo/Rustc worker 频繁对归档、对象文件和临时产物执行
`lseek(2)`。此前已经通过 perf 聚合计数确认，`OSFile::lseek()` 的实现阶段并不只是更新
open file description 的偏移；每次调用还会重新从 inode 构造路径、查询 `FsIndex` 的特殊
节点表，并在 miss 时再次查询 inode 类型。这些查询会进入 Ya2yOS 的 VFS/EXT4 共享路径，
在多 hart 编译时放大 lwext4 全局锁排队。

## `tmp_01.ans` 证据

维护者提供的 RISC-V、8 hart 约三分钟样本最后快照为 `t=157571ms`，Cargo 处于
`Building 7/446`，日志没有 panic、`TFAIL`、`TBROK` 或完成标记。最后一个 perf 汇总为：

```text
syscall_duration lseek:       32,811 samples, 15,820,616 us
lseek_duration impl:          32,919 samples, 15,571,456 us
lseek_duration type_check:    32,920 samples, 15,521,831 us
lseek_duration size:               6 samples,          16 us
lseek_duration sparse:             0 samples,           0 us
ext4_read_lock wait:                         120,554,954 us
ext4_read_lock hold:                          22,241,397 us
```

`type_check` 几乎等于 `lseek` 实现累计时间，而 `SEEK_END` 大小查询和稀疏文件探测都不是
主因。`ext4_read_lock` 的等待还明显高于持锁，说明这些重复的 metadata 操作会与并发
read 一起排队；但 lwext4 当前不是 SMP-safe，不能据此直接删除全局锁。

## 根因

Linux 在 `open(2)` 成功后把稳定的 inode/file 状态绑定到 `struct file`。后续 `lseek()` 只
检查该 open file description 的文件类型并更新 `f_pos`，不会重新进行 pathname lookup。

Ya2yOS 原来的 `OSFile::lseek()` 则在每次调用中执行：

1. `inode.path()`；
2. `FsIndex::special_node_type(path)`；
3. miss 时 `inode.types()`。

对 Cargo 常见的普通 regular file，这些结果在 open 后不会改变，因此属于可证明的重复工作。
FIFO、设备和 socket 又必须保持不可 seek 的 `ESPIPE` 语义，不能简单地把类型检查删除。

## 修复

文件 [os/src/fs/files/os_file.rs](../../../os/src/fs/files/os_file.rs) 的 `OSFile` 新增稳定
的 `seek_type: InodeType` 字段：

- `OSFile::new()` 和 `new_fanotify_event()` 创建 open file description 时调用一次
  `resolve_seek_type()`；优先使用 `FsIndex::special_node_type()`，否则回退到
  `inode.types()`。
- `lseek()` 只读取 `self.seek_type`，然后执行既有的 `SEEK_SET`、`SEEK_CUR`、`SEEK_END`、
  `SEEK_DATA` 和 `SEEK_HOLE` 逻辑。
- FIFO/socket 仍立即返回 `ESPIPE`；目录、普通文件、负偏移检查、文件大小查询和稀疏文件
  边界均未改变。

这与 Linux `struct file` 的生命周期相符：打开时解析一次稳定对象，热路径只更新偏移，
而不重复 pathname lookup。

## `tmp_02.ans` 验证

新的约三分钟 RISC-V 样本最后快照为 `t=149435ms`，Cargo 处于 `Building 5/446`，同样没有
panic、`TFAIL` 或 `TBROK`，但没有完成标记。最后汇总为：

```text
syscall_duration lseek:       31,895 samples,    187,462 us
lseek_duration impl:          32,002 samples,     10,833 us
lseek_duration type_check:         0 samples,          0 us
lseek_duration size:                5 samples,         22 us
lseek_duration sparse:              0 samples,          0 us
ext4_read_lock wait:                         141,573,230 us
ext4_read_lock hold:                          16,300,990 us
```

在近似的 `lseek` 调用量下，类型检查累计从 `15,521,831 us` 降为 `0`，实现累计从
`15,571,456 us` 降为 `10,833 us`；这直接验证了 open-time 类型缓存消除了原先的热路径
缺陷。不同样本的 Cargo 阶段、缓存状态和宿主调度不完全相同，所以不能把最终 `t=` 或
`Building N/446` 差异解释成完整 BuildStorm wall-clock 加速，也不能宣称性能已经超过 Linux。

剩余主要压力仍是 EXT4 全局锁排队：`tmp_02` 中 `read_lock` 等待累计约 `141.6 s`，远高于
约 `16.3 s` 的持锁累计（这些是多 hart 累计值，不是 wall-clock）。后续应继续按 read/find/
fstat/write 来源拆分锁等待，而不是放宽 lwext4 的 SMP 安全边界。

## 早期 `tmp_03.ans` 页缓存等待实验回归

本节保留的是页缓存 loading 实验版本的旧快照；维护者后来提供了同名的更新版
`tmp_03.ans`，其结果见下一节。不要把两次同名文件混为同一运行。

为对标 Linux page cache 的 locked folio 流程，曾临时尝试在 `FilePageCache` miss 时插入
`PAGE_LOADING` 占位，让同页并发访问者睡眠等待 loader，loader 完成后再发布页内容；
truncate/invalidate 也尝试唤醒等待者并重试。

维护者提供的 `tmp_03.ans` 在 `t=33511ms` 仍为 `Building 0/446`，之后三分钟窗口没有新的
perf 快照或 Cargo 推进。相比 `tmp_02.ans` 已到约 149 秒、`Building 5/446`，该实验存在
永久阻塞或任务生命周期/失效竞态，不能作为性能修复交付。相关页缓存实验代码已全部撤销，
当前工作区只保留安全的 `seek_type` 优化；在补齐 owner 取消、失效代际以及 read/mmap/splice
并发测试前，不重新启用该等待模型。

## 更新版 `tmp_03.ans`：混合命中读取造成的重复回源

维护者随后替换了 `tmp_03.ans`，该 RISC-V、8 hart 样本在 `t=175421ms` 推进到
`Building 5/446`，没有 panic、`TFAIL`、`TBROK` 或 `shutdown!`。最后快照显示：

```text
file_cache hit=453844 miss=24266
ext4 reads=32214 bytes=285835725
ext4_read_lock wait=311032683 us hold=29048081 us
ext4_find_lock wait=24303337 us hold=14515936 us
ext4_fstat_lock wait=8994652 us hold=10043656 us
ext4_write_lock wait=40857297 us hold=13317968 us
syscall_duration read=257989646 us read_active=153629671 us
syscall_duration write=54674286 us
syscall_duration path=42689348 us
```

这里的 `file_cache` 命中/缺页比例约为 18.7:1，但旧的 `OSFile::try_page_cached_read()` 对
大于一页的请求调用 all-or-nothing 的 `read_cached_at()`：只要跨页请求中有一页未命中，就
放弃所有命中结果，把整个用户请求重新交给 `inode.read_at()`。在 Cargo/Rustc 的并发读中，
这会重复读取已经驻留的页，并把本可避免的工作再次排到唯一的 lwext4 `EXT4_OP_LOCK`，与
`read_lock` 的 311 秒累计等待相符。

本轮修复保留页缓存的现有容量、失效、稀疏覆盖和两页预读策略，只改变大于一页的读拼装：

- 先按页复制已缓存内容；
- 将相邻未命中页合并为一个连续 `inode.read_at()` 请求；
- 只把完整覆盖的冷页插入 `FILE_PAGE_CACHE`，保持原有 partial-page 安全边界；
- 文件尾、特殊 inode、写入/截断/rename 失效和用户缓冲复制语义不变。

因此一次混合命中请求最多只为冷页段进入 EXT4，而不会因单页 miss 重读整段。当前没有
包含该修复的新 guest A/B，不能从旧版 `tmp_03` 宣称 wall-clock 加速；后续应重点比较
`ext4 reads`、`ext4_read_lock wait/hold` 和 `read_active`，并覆盖跨页、EOF、稀疏文件以及
并发 read/mmap/splice 回归。

## `tmp_04.ans`：混合命中读取修复后的方向性验证

`tmp_04.ans` 是混合命中读取修复后的 RISC-V、8 hart 样本。日志最后可见 Cargo 标记为
`Building 5/446`，最后一个 perf 快照在 `t=145163ms`、`Building 4/446`；没有 panic、
`TFAIL`、`TBROK`、`ERROR` 或 `shutdown!`。与旧版 `tmp_03.ans` 在约 145 秒的快照对齐：

| 指标 | `tmp_03.ans` (`t=143914ms`) | `tmp_04.ans` (`t=145163ms`) |
| --- | ---: | ---: |
| Cargo 阶段 | `Building 5/446` | `Building 4/446` |
| EXT4 reads | 28,732 | 27,174 |
| file-cache hit/miss | 420,136 / 21,397 | 382,838 / 22,167 |
| `ext4_read_lock` wait | 226.250 s | 122.490 s |
| `ext4_read_lock` hold | 24.141 s | 16.571 s |
| `read` total / active | 199.853 / 118.286 s | 150.207 / 49.550 s |
| `lseek` type_check | 0 us | 0 us |

在不同 Cargo 阶段和不同读写工作量下，`tmp_04` 没有出现混合命中修复应当消除的整段回源
特征，且 read 锁 wait/hold 和 read syscall 累计均较低；这与“只读取冷页段”的预期方向一致。
不过两个样本不是相同镜像状态、相同 Cargo DAG 位置和相同请求序列，不能将表中差异归因于
单一代码改动，也不能报告整体 BuildStorm wall-clock 加速或 Linux 超越结论。后续严格 A/B
应固定镜像、hart、内存、入口和缓存状态，并比较相同 Cargo 阶段下的 EXT4 reads、混合读的
冷段数量、read lock wait/hold 与 `read_active`。

## 验证与限制

已执行：

```text
make perf TARGET_ARCH=riscv64       # 通过
make perf TARGET_ARCH=loongarch64   # 通过
cargo fmt --manifest-path os/Cargo.toml -- --check  # 通过
git diff --check                    # 通过
```

构建只出现 vendored smoltcp 的既有 unused/dead-code warning。独立 QEMU A/B 尚未完成：当前
沙箱宿主的 `/var/tmp` 为只读，QEMU 在内核启动前无法创建临时文件；因此 `tmp_02.ans` 是维护
者提供的运行期验证，不能据此报告严格同镜像 wall-clock 百分比。完整 446 crate BuildStorm、
Linux 端到端对标和正式评分仍待后续同配置运行。
