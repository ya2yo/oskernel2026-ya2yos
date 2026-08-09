# BuildStorm 连续 bcache 写回请求合并

## 背景

`Docs/决赛文档/优化方案.md` 的 P0 统计已经将块设备请求同 lwext4 resource lock 类别关联。
`server.ans` 的中途 BuildStorm 样本显示 block queue wait、设备服务和 journal/inode 持锁 I/O
持续增长。更新后的约 10 分钟 `log.ans` 进一步确认，即使最后一个 interval 的 resource-lock
竞争已经很低，写请求碎片仍然突出。

## 现象

`log.ans` 最后一个 interval 位于 `t=590399ms`，持续 `185912ms`：

| 统计 | 数值 |
| --- | ---: |
| `interval_ext4_block_device.write_requests` | 146,045 |
| `interval_ext4_bcache_io.write_blocks` | 1,673,246 |
| 写入字节 | 856,701,952 |
| 块设备写服务累计 | 37,533,100 us |
| `direct_too_large_ops` / 字节 | 4,686 / 279,670,793 |
| journal resource-lock contention | 4 |

这些 duration 是并发累计值，不能相加为 wall-clock；但平均单次块写约 6 KiB，且
`journal` 类别覆盖上述所有写提交，满足 P2 的“小写或相邻请求未合并”前置证据。`server.ans`
此前在 `t=1382033ms` 的窗口也记录了 18,232 次块写、153,179,648 字节和明显的 block queue
wait/resource-lock wait，两个样本指向同一块层碎片方向。

两份日志均没有可用的完整最终结果：`log.ans` 没有 `BUILDSTORM_BEGIN` 后的完成标记，
`server.ans` 只有 `BUILDSTORM_BEGIN`；两者均未出现 `TFAIL`、`TBROK`、内核 panic、
`BUILDSTORM_FAIL`、`shutdown!` 或 summary。因此不能从这些中途样本计算加速百分比或声称
BuildStorm 已通过。

## 分析

原 `ext4_block_cache_flush()` 每次只通过 `ext4_bcache_claim_dirty()` 取一个未引用脏 buffer，
再调用 `ext4_block_flush_buf()`。后者无论后继 dirty buffer 是否具有相邻 LBA，都会调用：

```c
ext4_blocks_set_direct(bdev, buf->data, buf->lba, 1);
```

底层接口已经支持 `cnt > 1` 的连续逻辑块请求，但该全局 flush 未使用它。bcache dirty list
按最近写入插入表头，因此顺序写通常形成反向连续 LBA；逐 buffer 写回会把这一可合并范围拆成
多次设备提交。

不能对所有 dirty buffer 任意排序或合并。`end_write` 被 journal checkpoint 用于回收
`jbd_buf`、transaction 和 block record；回调可递归消费 descriptor。改变 callback 之间的
完成顺序会破坏现有 journal 生命周期和 cache flush 串行化修复。

## 根因

bcache 全局 flush 缺少针对连续、无 callback dirty block 的批处理路径。即使块设备能够接受
多块连续请求，当前实现仍固定以 `cnt=1` 写回，从而在 BuildStorm 直接写和 cache flush 场景中
产生大量小请求与设备服务开销。

## 修复

- 在 `ext4_block_cache_flush()` 中最多预取 32 个 dirty block；仅接受同方向连续的 LBA。
  首个不连续块或带 `end_write` 的 buffer 保留为下一轮 pending block，不跨越其边界。
- 对 count 大于 1 的无 callback 批次，用临时有序缓冲区将 dirty-list 的正向或反向 LBA 排成
  连续数据，再调用一次 `ext4_blocks_set_direct(..., first_lba, count)`。
- 每个 batch member 仍独立设置/清除 `BC_WRITEBACK`、记录既有 writeback telemetry、唤醒 waiter；
  只有整次设备提交返回 `EOK` 才 `mark_clean`。错误时所有 buffer 仍为 dirty，可按原逻辑重试。
- 临时缓冲区分配失败时回退原来的逐 buffer `ext4_block_flush_buf()`，不向用户路径新增 `ENOMEM`
  失败语义；带 journal callback 的块始终使用原路径。
- `bcache_lifecycle` 增加回归：4 个连续 dirty block 只提交 1 次设备写；批量 `EIO` 后磁盘内容
  保持旧值、bcache oracle 通过，重试后才写入新内容。

## 涉及文件

- `crates/lwext4_rust/c/lwext4/src/ext4_blockdev.c`
- `crates/lwext4_rust/c/lwext4/fs_test/bcache_lifecycle.c`
- `Docs/决赛文档/优化方案.md`

维护者已有的 `.vscode/settings.json`、根 `Makefile`、`user/src/bin/initproc.rs` 和未跟踪
`disk.img` 未被本轮修改或回退。

## 验证

- `cmake -S crates/lwext4_rust/c/lwext4 -B /tmp/ya2yos-lwext4-bcache-test -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DLIB_ONLY=OFF -DLWEXT4_USE_USER_MALLOC=OFF -DLWEXT4_BLOCK_CACHE_SIZE=2048`：配置通过。
- `cmake --build /tmp/ya2yos-lwext4-bcache-test --target lwext4-bcache-lifecycle -j2`：通过。
- `/tmp/ya2yos-lwext4-bcache-test/fs_test/lwext4-bcache-lifecycle`：输出
  `lwext4-bcache-lifecycle: PASS`，覆盖连续批量、失败重试、callback ownership、并发加载与
  bcache invariant。
- `make TARGET_ARCH=riscv64`、`make TARGET_ARCH=loongarch64`：release 构建通过。
- `make TARGET_ARCH=loongarch64 perf`：包含 `EXT4_PERF_TELEMETRY` 的 perf 构建通过。
- `git diff --check`：通过。

未运行 QEMU、CAgent、LTP 或完整 BuildStorm。根 `make run` 会先删除未跟踪的 `disk.img`，而
它当前指向维护者的正式 LoongArch64 镜像；为避免改变工作树状态，本轮保留该链接。后续应在
相同镜像、Hart 数、`initproc` 开关和 perf 配置下重复 BuildStorm，比较写请求数/写块数、
`service_us`、`wait_us`、文件校验和最终完成标记，再报告性能结论。
