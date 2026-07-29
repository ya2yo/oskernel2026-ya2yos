# RISC-V pipe 跨 hart 空闲唤醒延迟优化

## 背景

维护者提供的 `tmp_02.ans` 已使用 lmbench glibc `bw_pipe -P 1` 独立入口，完整输出
`Pipe bandwidth: 40.90 MB/sec`、测试组 END 和 `shutdown!`。本轮以相同 RISC-V、8 hart、
8 GiB、同一预制镜像和 `perf` 内核配置追踪该 pipe-only workload。

## 现象

`tmp_02.ans` 最终快照累计传输约 `2.16 GiB`，没有成功 short read/write：

```text
pipe_io: read_bytes=2160066749 write_bytes=2160132285
pipe_duration:
  read_wait  samples=4776 total_us=50703149
  write_wait samples=4729 total_us=50424521
  read_copy  samples=32966 total_us=1646668
  write_copy samples=32967 total_us=4555589
```

read/write 的累计等待分别约 50 秒，而两侧 copy 合计约 6.2 秒。两个等待值来自并发任务，
不能相加解释为单线程耗时；但单次等待平均约 10.6 ms，接近内核的 100 Hz timer 间隔，明确
指向调度唤醒延迟而非 EXT4 锁或数据复制。

## 分析

`bw_pipe` 的 reader 是 lmbench 主进程，writer 会在 `bw_pipe.c` 内再次 `fork()`。普通
`fork` 由 `Process::new()` 按 `(pid - 1) % HART_NUM` 分配 `home_hart`，所以这两个 pipe
端点常驻不同 hart，而非同一 hart。

pipe 写入/读取唤醒对端时，`PipeRingBuffer::wake_waiters()` 将任务标为 `Ready` 后调用
`ready_queue::add_task()`，CFS 会正确放入目标 hart 的 run queue；旧路径没有通知该目标
hart。目标没有运行任务时，RISC-V `idle()` 在 `wfi` 中只能由下一次 one-shot timer 返回，
因而 pipe 的 producer/consumer 交接被量化为约一个 10 ms tick。

曾试验在 pipe 满/空时由当前调用方执行 `yield_current_and_run_next()`。在同配置样本中带宽
从 `40.90` 降至 `14.09 MB/sec`，且 wait 没有下降；该尝试会强制当前 hart 进行额外切换，无法
唤醒另一个 hart 的 idle CPU，已完整撤回，不作为最终方案。

## 修复

将优化放入通用 scheduler 入队路径，而不在 pipe 特化调度策略：

- `processor` 为每个 hart 增加原子 idle 标志。目标 hart 在确认 run queue 为空后先发布 idle，
  再二次检查队列才进入架构 idle；远端 enqueue 要么被二次检查观察到，要么观察到 idle 标志并
  发出通知，消除 enqueue 与 `wfi` 之间的丢唤醒窗口。
- `ready_queue::add_task()` 入队完成后通知目标 hart。仅当调用者和目标 hart 不同，且目标已
  发布 idle 时，RISC-V 通过 SBI sPI `send_ipi()` 发送 supervisor software interrupt。
- RISC-V `idle()` 只在 `wfi` 周期临时开启 supervisor software interrupt，并在恢复后立即
  清除，使 IPI 不进入用户 trap 路径，也不会抢占正在运行的远端任务。
- LoongArch64 当前 idle 是自旋而非 `wfi`，下一轮调度循环即可看到队列，保留统一接口但不发送
  IPI，未扩大为未验证的 LoongArch IPI trap 修改。
- 新增 `scheduler_wakeup` 聚合统计，记录 local/remote enqueue、远端 idle 通知以及 IPI
  成功/失败数；既有 pipe 的 wait/copy/short/wakeup 统计保持不变。

## 涉及文件

- `os/src/task/processor.rs`
- `os/src/task/scheduler/mod.rs`
- `os/src/arch/riscv64/qemu/cpu.rs`
- `os/src/arch/loongarch64/qemu/cpu.rs`
- `os/src/utils/perf.rs`
- `user/src/bin/lmbench/mod.rs`

## 验证

- `cargo fmt --manifest-path os/Cargo.toml -- --check`、
  `cargo fmt --manifest-path user/Cargo.toml -- --check` 与 `git diff --check` 通过。
- `make perf TARGET_ARCH=riscv64` 和 `make perf TARGET_ARCH=loongarch64` 通过；仅有既有
  Cargo config 弃用与 vendored `smoltcp` warnings。
- 两次 RISC-V QEMU 运行均执行同一 glibc `bw_pipe -P 1` 入口，均有测试组 END、
  `bw_pipe_exit_status=0` 与 `shutdown!`，无 `panic`、`TFAIL`、`TBROK` 或 timeout。

| 样本 | Pipe bandwidth | read wait | write wait | remote IPI |
| --- | ---: | ---: | ---: | --- |
| `tmp_02.ans` 基线 | 40.90 MB/sec | 50.703 s / 4776 | 50.425 s / 4729 | 未实现 |
| `/tmp/tmp02-ipi-1.ans` | 251.96 MB/sec | 0.969 s / 3281 | 0.141 s / 312 | 3617 sent, 0 failed |
| `/tmp/tmp02-ipi-2.ans` | 223.03 MB/sec | 1.411 s / 4005 | 0.136 s / 338 | 4359 sent, 0 failed |

两次优化样本相对基线为约 `5.45x` 至 `6.16x`。它们使用同一测试入口和镜像，但仍可能受 QEMU
宿主调度与缓存状态影响，故报告区间和原始统计，不将单一数值外推为完整 lmbench、BuildStorm 或
LoongArch64 的性能结论。

## 剩余风险

- IPI 仅用于已经 idle 的远端 hart，不实现 running hart 的 wakeup preemption；高负载下 remote
  runnable task 仍由正常 timer 抢占调度，这避免未经完整回归验证的跨 hart 强制抢占。
- LoongArch64 只完成编译验证。其内核 IPI trap 仍未实现，且当前 spin-idle 不需要该 IPI 才能
  观察新任务，不能将 RISC-V QEMU 数据外推到该架构。

## 大消息 copy 路径收敛

### 细分取证

在保留 idle IPI 的同一 RISC-V、8 hart、8 GiB、预制镜像和 glibc `bw_pipe -P 1` 配置下，
先增加四个 `pipe_duration` 聚合项。`/tmp/tmp02-copy-breakdown.ans` 完整结束，带宽为
`233.68 MB/sec`，首个约 `2.14 GiB` 快照的 large-message 数据路径如下：

| 阶段 | samples | total_us | 含义 |
| --- | ---: | ---: | --- |
| `read_pipebuf_gather` | 32640 | 993290 | `PipeBuf` 片段聚合为临时数据 `Vec` |
| `read_user_copy` | 32640 | 573207 | 聚合数据 `Vec` 回写 `UserBuffer` |
| `write_user_extract` | 32640 | 3411734 | `UserBuffer::read()` 生成临时数据 `Vec` |
| `write_pipebuf_copy` | 32640 | 1201123 | 临时数据 `Vec` 再复制为最终 `PipeBuf` |

同一快照中 `read_copy/write_copy` 为 `1.615/4.657 s`。因此临时数据 `Vec` 确实处于热路径：
read 需要先聚合再回写，write 则对同一字节流进行了两次分配/复制。该结论来自完整 workload，
而不是把并发 wait 累计值误解为单线程执行时间。

### 实现

- `UserBuffer::write_from_slices()` 顺序推进源片段与用户页片段的偏移，直接完成
  `PipeBuf -> UserBuffer` 的一次数据复制。
- 大消息 read 改用 `PipeRingBuffer::read_into()`。它仍通过既有 `pop_bufs()` 移出或切分
  `PipeBuf`，从而保留 splice/tee 的片段和容量语义；仅保留短生命周期的 `Vec<PipeBuf>` 元数据，
  不再分配按数据量大小的 gather `Vec<u8>`。
- 大消息 write 只分配最终的 `Vec<u8>`，用既有 `UserBuffer::read_to()` 填充后交给
  `write_owned_bytes()` 移入 `PipeBuf`。`PipeBuf::new()` 接收该 `Vec` 的所有权，不再执行
  `to_vec()` 的第二次数据复制。
- `<= 10` 字节的原有逐字节路径、available-read/write 边界、short read/write 返回值、等待队列、
  wakeup、锁域及 pipe 容量均未改变。`read_into()` 和 `write_owned_bytes()` 返回实际处理长度，
  仍由调用方作为系统调用结果记录。

`read_pipebuf_gather` 和 `write_pipebuf_copy` 统计保留，方便跨版本对照；优化后前者为零，后者只测量
`Vec` 所有权移入 `PipeBuf` 的元数据开销，不再代表数据复制。

### 验证

两次独立 RISC-V QEMU 运行均包含测试组 END、`bw_pipe_exit_status=0` 与 `shutdown!`，未出现
`panic`、`TFAIL`、`TBROK` 或成功 short read/write，`remote_ipi_failed=0`：

| 样本 | 带宽 | read copy | write copy | read gather | PipeBuf 插入 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `/tmp/tmp02-copy-breakdown.ans` | 233.68 MB/sec | 1.615 s | 4.657 s | 0.993 s | 1.201 s |
| `/tmp/tmp02-copy-opt-1.ans` | 264.51 MB/sec | 0.783 s | 3.695 s | 0 | 0.058 s |
| `/tmp/tmp02-copy-opt-2.ans` | 258.19 MB/sec | 0.760 s | 3.695 s | 0 | 0.054 s |

相对细分基线，两个优化样本的 read copy 降低约 `51.5%`、`52.9%`，write copy 均降低约 `20.7%`；
带宽分别提高约 `13.2%` 与 `10.5%`。`write_user_extract` 仍约 `3.61 s`，符合保留的一次用户页到
最终 pipe 数据的必要复制。QEMU 吞吐仍可能受宿主调度影响，故这里只报告两个完整同配置样本的区间，
不外推为全量 lmbench、BuildStorm 或 LoongArch64 的性能结论。

本轮还通过 `cargo fmt --manifest-path os/Cargo.toml -- --check`、`git diff --check`、
`make perf TARGET_ARCH=riscv64`、`make perf TARGET_ARCH=loongarch64` 以及两个架构的普通 release
构建。LoongArch64 未运行 QEMU 行为回归，仅完成编译验证。
