# BuildStorm EXT4 gate owner 退出泄漏

## 背景

lwext4 的 block cache 与 path/file C API 不是 SMP-safe，Ya2yOS 用挂载级
`EXT4_OP_LOCK` 将所有 lwext4 操作串行化。P18.1 已将这把 gate 改为 FIFO ticket
交接，并在任务退出时清理排队票据。

## 现象

`tmp_03.ans` 是 RISC-V 8 HART、约 55.4 分钟的未完成 BuildStorm 样本。日志只有
`BUILDSTORM_TOOLCHAIN ok` 和 `BUILDSTORM_MINIBUILD ok`，没有编译完成标记、测试组 END
或 `shutdown!`，末尾为 `QEMU: Terminated`，没有 panic/TFAIL/TBROK/SIGSEGV。

在 `t=3021715ms` 后，syscall 计数几乎不再增长；到 `t=3075042ms`，所有
`interval_ext4_lock` 和 `interval_ext4_gate` 的新增计数均为零，但 gate 队列仍有 6 个
waiter。末尾 gate 统计为：

```text
queued=46288 handoffs=46281 cancelled=0 queue_depth=7
```

因此 `queued = handoffs + cancelled + queue_depth` 仍然成立，但队列中的任务没有继续
取得 gate。页缓存仅为 `85486/196608` 页且 `capacity_bypass_pages=0`，不能把这次停滞归因于
页缓存容量。

## 分析

`exit_current_and_run_next()` 是发散路径：它取下当前任务并切换调度，不会沿普通 Rust
栈返回，因此活跃的 `Ext4OpGuard` 不一定执行 `Drop`。旧退出清理只按 TID 移除
`Ext4OpLockFuture` 的排队 ticket；如果退出任务已经持有 gate，`Ext4OpState.held`
可能永远保持为 `true`，后续 FIFO waiter 只能睡眠。

`tmp_03` 证明了“队列守恒但永久停滞”的现象，与该生命周期缺口一致，但没有单独证明
每一个停滞样本都由 owner 退出触发。因此该结论作为可证伪的防御性假设，下一轮通过 owner
诊断字段确认；若 owner 已清零而队列仍非空，应转查队首 waker/ready queue。

## 修复

- `Ext4OpState` 增加 `owner_tid`；fast path 和 FIFO handoff 获取 gate 时记录 owner。
- `cancel_ext4_op_waiter(tid)` 同时处理排队 waiter 和当前 owner。owner 退出时清除
  `held/owner_tid`，并唤醒 FIFO 队首。
- 正常 release 在逻辑状态锁内先清除 owner，再唤醒队首，避免 perf owner 快照被并发
  fast acquire 覆盖。
- perf 报告增加 `owner_tid`、`owner_hold_us`、`owner_exit_releases`。下一轮仍检查
  `queued = handoffs + cancelled + queue_depth`。

涉及文件：

- `os/src/fs/ext4_lw/mod.rs`
- `os/src/utils/perf/fs.rs`
- `os/src/utils/perf/report.rs`
- `os/src/task/mod.rs`（已有退出汇合点调用 `cancel_ext4_op_waiter`）

## 验证

- `cargo fmt --manifest-path os/Cargo.toml --all -- --check`
- `git diff --check`
- RISC-V/LoongArch64 `make perf`
- RISC-V/LoongArch64 release `make TARGET_ARCH=riscv64` / `make TARGET_ARCH=loongarch64`
- `timeout 120s make run TARGET_ARCH=riscv64`：启动 8 HART，完成
  `BUILDSTORM_TOOLCHAIN/MINIBUILD` 并进入 Cargo `4/446`，无 panic/TFAIL/TBROK；外层 timeout
  退出码为 124。

以上构建和静态检查通过，仅有 vendored smoltcp 的既有 unused import/dead-code warning。
完整 BuildStorm、64+ waiter/队首退出定向回归以及新字段的运行期复现尚未完成；因此不能
宣称已经消除所有停滞或获得端到端加速。
