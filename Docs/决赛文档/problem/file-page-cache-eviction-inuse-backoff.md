# BuildStorm 文件页缓存满载候选退避

## 背景

全局文件页缓存采用固定的 `192 * 1024` 页生产上限；显式 feature
`file-cache-capacity-test` 将上限降为 2,048 页，用于在较短的 RISC-V QEMU 运行中放大满载行为。容量满时，
`FilePageCache` 只能回收 clean、未被调用方或 mmap VMA 引用的页；否则保持已有的 capacity-bypass 语义，
让本次加载继续完成但不发布到全局缓存。

此前已修复 bypass 页在 mmap 安装时被丢弃并导致动态 ELF SIGSEGV 的功能错误。本问题只处理安全淘汰器在
候选长期 `in_use` 时的 CPU 开销，不以调整容量或掩盖 rustc 异常为目标。

## 现象

旧版 clean-page CLOCK 只有一条候选队列。扫描到 `in_use` 或 dirty 页后，代码把该键直接放回队尾；缓存完全由
mmap VMA 或调用者持有时，下一次 cold miss 又会扫描相同的候选。每次满载请求的扫描预算固定为 128 项。

`log.ans` 的 2K 压力样本显示，早期仍可回收一部分页：

```text
t=31.289s  bypass=25790  evictions=12972  scans=3240417  in_use=3210743
t=95.666s  bypass=76808  evictions=16316  scans=9685492  in_use=9647610
t=261.966s bypass=202163 evictions=16316  scans=25690228 in_use=25650541
```

`evictions` 在 16,316 后停止增长，而末尾扫描中约 99.85% 是 `in_use` 跳过。也就是说，没有安全页可回收时，
capacity bypass 本身以外又引入近似 `128 * miss` 的无效索引、引用计数与队列操作。

同一旧样本约在 `t=161.948s` 后出现 `rustc interrupted by SIGSEGV`；日志没有 fault VA/scause，且回溯中
`0xffffffc080202000` 对应启动日志中的 `sigreturn_trampoline`，不能据此把 SIGSEGV 归因于 CLOCK 代码或页帧回收。

## 分析

`FilePage` 的 `referenced` 位只适合表达最近缓存命中，`in_use` 则说明 `FilePage` 或其 `FrameTracker` 仍被缓存外
持有。特别是文件 mmap VMA 保存 `page.frame.clone()`，因此仅检查 `Arc<FilePage>` 计数不足以判断是否可回收。

当已扫描过的页仍为 `in_use` 时，短期内再次检查通常没有价值；但永久丢弃候选也不正确，因为 VMA 解除或调用者
释放 `Arc` 后，这些页应当重新成为可回收页。所需策略是将近期不可回收的页从正常选择集合中移走，并以有限频率
重新抽样，而不是每次 miss 对全体做同步重试。

## 修复

`FilePageCache` 将单一 `eviction_candidates` 替换为只保存 `FilePageKey` 的 `EvictionQueues`：

- `active`：新发布页和获得 CLOCK 二次机会的页；每次压力最多扫描 128 项。
- `deferred`：扫描时为 dirty 或 `in_use` 的页；不保留 `Arc<FilePage>`/frame，因此不会人为阻止回收。
- active 清空且 deferred 非空时，设置 256 次容量 miss 的冷却期；冷却到期后仅 FIFO 移动 16 项回 active。
- 冷却期间直接返回“本次无可回收页”，调用方继续原有 capacity bypass；失效路径同时从两条队列删除键。

所有同时访问页索引和候选队列的路径仍遵守 `pages -> eviction_queues` 锁序。淘汰条件保持为 clean、
`Arc<FilePage>` 无外部引用且 `FrameTracker` 无外部引用；不进入 lwext4，不在 `MemorySet` 写锁内做文件 I/O。

新增 perf 字段：

- `eviction_deferred_retry_pages`：冷却到期后重新进入 active 的候选数。
- `eviction_cooldown_bypasses`：active 为空时因冷却直接 bypass 的次数。

它们与已有 `eviction_scans`、`eviction_in_use_skips` 一起区分“没有扫描”与“统计遗漏”。

## 验证

使用新实现显式构建 `perf,file-cache-capacity-test` 并运行 60 秒 RISC-V QEMU，得到：

```text
t=33.816s
resident_pages=2048 max_pages=2048 capacity_bypass_pages=35318
evictions=4601 eviction_scans=17232 eviction_second_chances=8567
eviction_in_use_skips=4064 eviction_deferred_retry_pages=2016
eviction_cooldown_bypasses=32414
```

扫描均摊低于 0.49 次/bypass，而旧样本对应窗口约为 125.6 次/bypass；`cooldown_bypasses` 的增长证明大多数
满载 miss 未重扫 128 项。运行只达到 `BUILDSTORM_TOOLCHAIN ok`，由外层 60 秒 timeout 结束；未出现 panic，
但没有到达旧 SIGSEGV 发生阶段，不能声称 rustc SIGSEGV 已修复，也不能据此给出完整 BuildStorm wall-clock
结论。

已执行：

- `cargo fmt --manifest-path os/Cargo.toml --all -- --check`
- `git diff --check`
- `make perf TARGET_ARCH=riscv64`
- `make perf TARGET_ARCH=loongarch64`
- `make TARGET_ARCH=riscv64 special_make KERNEL_EXTRA_FEATURES=perf,file-cache-capacity-test`
- `timeout 60s make run TARGET_ARCH=riscv64 > /tmp/file-page-cache-deferred-riscv.log 2>&1`

下一次长测应继续使用同一 feature 直到验证 `eviction_scans` 不再随 bypass 线性放大，并保留 rustc fault 的
内核异常证据；完成后恢复正式 `make perf TARGET_ARCH=riscv64` 产物。

## 涉及文件

| 文件 | 修改 |
| --- | --- |
| `os/src/fs/page_cache.rs` | 双队列候选选择、冷却和有限批量复检；同步更新失效路径。 |
| `os/src/utils/perf/fs.rs` | 新增 deferred retry 与 cooldown bypass 聚合计数。 |
| `os/src/utils/perf/report.rs` | 输出新计数到 `file_cache_capacity` 报告。 |
| `Docs/决赛文档/优化方案.md` | 以退避策略替换容量 A/B 主线，记录验证边界。 |
