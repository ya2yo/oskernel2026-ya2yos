# BuildStorm EXT4 稀疏写缓冲 run 合并

## 背景

稀疏 regular inode 不能使用仅保存字节的 whole-file write-back cache：回写整段
缓存会把原本的 hole 物化。`crates/lwext4_rust/src/file.rs` 因而使用按
`(mountpoint, inode)` 索引的 sparse write buffer，在真正调用 `ext4_fwrite()` 前保存
原始 offset 的小块写入。

维护者提供的 LoongArch64 `server.ans` 是一次被人工终止的 BuildStorm 运行。CAgent、
toolchain 和 minibuild 已完成，但没有 `BUILDSTORM_COMPILE`、结束标记或 `shutdown!`，
故它只能定位中途热点，不能作为完整通过结果或端到端 A/B 基线。

## 现象

最终 perf 快照的 sparse buffer 累计统计如下：

| 指标 | 值 |
| --- | ---: |
| flush batches | 7,634 |
| flush batch runs | 236,719 |
| flush batch bytes | 285,332,888 |
| `run_limit_batches` | 7,363 |
| `payload_limit_batches` | 266 |
| `global_budget_batches` | 0 |

约 `96.4%` 的 batch 因 run 上限而提交，平均每 batch 约有 `31.0` 个 run，已经非常接近
32-run 容量。运行早期的代表性快照也在约 57 秒时出现 `sparse_buffer_ops=1837`、
`batches=32`、`run_limit_batches=24`，说明后续验证可以先用短时预构建观察该路径，
不必等待完整 BuildStorm 结束。

## 根因

旧逻辑只会把“新写入 offset 严格等于既有 run 末尾”的写入追加到该 run。以下仍会新增 run：

- 新写入接在既有 run 的前方；
- 新写入覆盖或部分覆盖既有 run；
- 新写入桥接两个已有 run。

Rustc/链接器对同一 artifact 的交错小写经常出现这些排列。因此许多本可由一个连续
物理写提交的范围累计为 32 个 run，随后在容量边界被迫进入 `ext4_fwrite()` 批量回写，
重新排队到 lwext4 全局操作锁。

## 修复

`crates/lwext4_rust/src/file.rs` 将两个 sparse 写入口收敛到同一个插入 helper，并对每次
新写入执行以下处理：

1. 以新写入区间为起点，迭代选择所有重叠或端点相接的已有 run，直到连通区间不再扩张。
2. 预先分配合并后的 `Vec`；只有所有会失败的分配都成功后，才修改 `VecDeque`。
3. 按已有 run 的原始写入顺序复制，再复制本次写入，保持 `pwrite` 风格的
   last-write-wins 语义。
4. 只替换这个连通分量。中间仍有 hole 的两个 run 保持独立，flush 时仍在各自原始 offset
   调用 `ext4_fwrite()`，不会合成零填充范围。
5. 以合并后实际驻留字节替换 inode/global sparse 预算，而不是无条件累加本次写入长度。

`try_buffer_sparse_write_at()` 仍不会触发 flush；容量或分配压力只返回 `false`，由已经持有
lwext4 gate 的常规写路径决定如何发布。`buffer_sparse_write_at()` 保留原有的“当前 inode
flush 后重试一次”行为。idle 控制流、pipe、block cache 容量和 remote-TLB 协议均不在本轮范围内。

## 回归覆盖

在 `file.rs` 增加了不依赖 lwext4 设备的内存单元测试，覆盖：

- 前向和后向相邻写的合并；
- 一个新写入桥接两个 run；
- 重叠范围的 last-write-wins；
- hole 两侧保持为两个 run；
- 达到 32 run 后，新的不相交范围被拒绝，但可连接两个已有范围的写仍能合并并降低 run 数；
- 重叠合并后 inode/global 预算仍按实际 union 字节计数。

## 验证

按维护者要求，本次修改期间未运行 `cargo test`、`make`、QEMU、CAgent、LTP 或 BuildStorm，
避免干扰正在进行的全量测试。因此目前没有编译、运行期语义或性能实测结果，也不能报告具体
加速百分比。

全量测试结束后，建议先执行新增的纯内存 sparse buffer 单元测试，再以与 `server.ans` 相同的
LoongArch64 BuildStorm 配置进行至少两次同阶段对比。重点比较完成阶段 wall-clock、
`run_limit_batches / batches`、`batch_runs / batches`、batch 数及 `ext4_write_lock` 的
wait/hold；只有确认功能日志无 `panic`、`TFAIL`、`TBROK` 且两个独立样本趋势一致后，才计算
端到端收益。
