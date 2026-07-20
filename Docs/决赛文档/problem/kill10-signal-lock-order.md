# LTP kill10 信号投递锁序反转风险

## 背景

LTP `kill10` 会创建两组 manager/child 进程，并高频互发 `SIGALRM`，同时依赖
`SA_SIGINFO` handler 完成同步。RISC-V 全量测试的 `riscv.ans` 在 `RUN LTP CASE
kill10` 后只留下启动参数，没有出现 LTP summary 或超时结果；单独运行则正常。

## 现象

原始 `riscv.ans` 在此前的 kill 测例均完成后停在：

```text
RUN LTP CASE kill10
["kill10\\0"]
```

日志没有保存卡死时的 hart 回溯，因此不能仅凭该文件证明某一锁链是唯一实际死锁
根因。但 `kill10` 的高频跨 hart 信号投递与 handler 执行会放大信号路径中的锁序
问题，必须先消除确定的反转。

## 分析

项目锁序规定 `TaskControlBlockInner` 位于 `SigTable` 之前的层级，持有任务锁时不
得再进入信号表。旧代码有两处违反该规则：

1. `add_signal_with_info()` 先获取目标 `TaskControlBlockInner`，再读取 `SigTable`
   判断该信号是否应唤醒可中断等待。
2. trap return 的 `handle_signal()` 先获取当前 `TaskControlBlockInner`，再读取
   `SigTable` 取得 handler/default disposition。

两条路径均处于 `kill10` 的 `SIGALRM` 投递与处理闭环。它们会把任务锁覆盖到信号
表锁，扩大与并发信号处理、action 更新和退出路径的互锁窗口，违反内核已声明的
锁序约束。

## 根因

信号 disposition 查询被放入 `TaskControlBlockInner` 临界区，导致
`TaskControlBlockInner -> SigTable` 的反向嵌套。该嵌套不符合 `os/src/task/mod.rs`
的全局锁序，属于即使未稳定取得锁栈也应修复的死锁风险。

## 修复

- `add_signal_with_info()` 先在无任务锁状态下快照 signal action 并计算
  `interrupt_wait`，再短暂获取任务锁写入 pending 位和状态。
- `handle_signal()` 同样先快照 signal action，再获取任务锁消费 pending signal 与
  对应 `siginfo`。

因此 `SigTable` guard 在进入 `TaskControlBlockInner` 前已释放；pending 更新、停止态
恢复和 blocked task 唤醒的原有行为保持不变。action 与投递并发变化时沿用快照语义，
这是标准信号不排队实现中可接受的投递时刻视图。

涉及文件：

- `os/src/signal/delivery.rs`
- `os/src/signal/pending.rs`

## 验证

已执行：

```text
make TARGET_ARCH=riscv64
timeout 180s make run TARGET_ARCH=riscv64 > /tmp/kill10-signal-lock.log 2>&1
git diff --check
```

结果：

- RISC-V 构建通过；根 `Makefile` 的构建链也完成 LoongArch64 编译，只有既有 Cargo
  config 弃用提示和 vendored `smoltcp` warnings。
- RISC-V 双 hart QEMU 中，musl 与 glibc `kill10` 均输出
  `TPASS: All 2 pgrps received their signals`，summary 均为 `passed 1 failed 0 broken 0`，
  最终 `shutdown!`。
- `git diff --check` 通过。
- LoongArch64 本次未运行 `kill10` 行为回归，仅完成编译。
