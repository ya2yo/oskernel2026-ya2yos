# BuildStorm 创建元数据连续锁交接优化

## 背景

`tmp_15.ans` 与二次运行的 `log.ans` 都是同一入口的十分钟窗口，用户要求以这两份输出作为当前性能判断依据。

## 现象

两份日志都只输出 `BUILDSTORM_TOOLCHAIN ok` 和 `BUILDSTORM_MINIBUILD ok`，没有完整的
`BUILDSTORM_COMPILE`、测试组结束标记或 `shutdown!`。末尾可见 Cargo 进度分别为 `41/446`
（`tmp_15`，guest 约 `557.5 s`）和 `38/446`（`log.ans`，guest 约 `543.1 s`）。

## 分析

两份样本的 sparse 工作量完全相同：`batch_bytes=34,922,007`、`88` 个 batch、最大 payload
`524,288 B`、最大 `32` runs，预算和分配失败均为零。因此 sparse 缓冲不是本轮波动来源。

稳定热点仍在 EXT4 串行元数据和读路径。`tmp_15`/`log.ans` 最终快照的 create 累计约
`39.49/44.14 s`，mode 约 `46.30/53.30 s`，owner 约 `21.52/26.12 s`；读锁等待约
`580.38/593.47 s`，fstat 锁等待约 `145.54/230.57 s`。同时 page-cache `load_races`
为 `40/88`，说明 Cargo/Rustc 的并发交错不同。`Building N/446` 因而受 QEMU TCG、宿主
调度和 lwext4 全局锁队列影响，不能单独证明内核回归或加速。

创建路径 `create_file()` 原先连续执行 `parent.create()`、新 inode `fmode_set()`、
`owner_set()`。新 inode 尚未发布到 `FsIndex`，但每个步骤都会重复进入 Rust inode 状态锁和
挂载级 EXT4 gate；mode/owner 的 C 调用本身仍必须串行，能安全减少的是连续锁交接。

## 根因

创建后的初始 mode/owner 更新属于同一个新 inode 的连续初始化阶段，却被拆成三个独立的
VFS 调用。全局 gate 交接会让并发 BuildStorm 任务重新排队，放大调度和锁等待波动；同时
两份十分钟日志的工作阶段并不完全相同，不能从累计秒数直接计算优化比例。

## 修复

- 在 `Inode` trait 增加 `create_with_metadata()` 默认实现，其他后端保持原有调用顺序。
- `Ext4Inode` 覆盖该入口，在一次 `lock_for_namespace()` 持有期间完成创建、mode 和可选
  owner 更新；新 inode 未发布，直接访问其 `Ext4File`，不重复获取子 inode 锁。
- `create_file()` 在创建前计算 umask、目录 setgid mode 和 owner tuple，再调用组合入口。
- 保留 create/mode/owner perf phase、失败后的 `recover_live_path()` 重试、目录 epoch、
  stat cache 失效、mode 先于 owner 以及原有错误传播语义。

## 验证

- `cargo fmt --manifest-path os/Cargo.toml`
- `make TARGET_ARCH=riscv64 perf`
- `make TARGET_ARCH=loongarch64 perf`
- `make TARGET_ARCH=riscv64`
- `make TARGET_ARCH=loongarch64`
- `git diff --check`
- 提升权限的 `timeout 120s make run TARGET_ARCH=riscv64` 启动成功，推进到 `3/446`，无
  `panic/TFAIL/TBROK`；因外层 timeout 结束，未作为性能 A/B。

`tmp_15.ans` 和 `log.ans` 早于本修复构建，后续仍需使用同镜像、同内存/SMP、独占宿主条件
重复完整窗口，并比较最终 perf 快照和完整 BuildStorm 结束标记。
