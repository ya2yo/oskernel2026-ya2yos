# 定时器无竞争抢占的调度路径优化

## 背景

cagent 长时间运行的 guest 统计显示，调度器选取次数很高。调度器原本在每次
timer interrupt 中都进入完整的 `suspend_current_and_run_next()` 路径，即使当前 hart
没有其他可运行任务。

## 现象

当一个 hart 只有当前任务运行时，timer interrupt 仍会把当前任务标记为 `Ready`，切换
到 idle 上下文，再由 `run_tasks()` 将同一个任务重新取出并切回。这个过程不会改变用户
可见语义，但会额外执行任务状态更新、就绪队列加锁、调度选择和上下文切换。

## 分析

定时器路径位于 `os/src/trap/mod.rs`：处理 timer、futex 和 itimer 事件后，原实现无条件
调用 `suspend_current_and_run_next()`。实际调度策略已经按 hart 维护 CFS 队列，RR 则
使用全局队列并依据 `home_hart` 过滤任务；当没有竞争任务时，调度并不存在需要解决的
选择问题。

## 根因

抢占入口把“时间片到期”和“必须切换任务”视为同一条件，没有先检查当前 hart 是否有
候选就绪任务，导致无竞争场景也经过 idle 调度往返。

## 修复

- 在 `os/src/task/mod.rs` 增加 `preempt_current_and_run_next()`。入口先检查进程级退出
  和不可忽略的 `SIGKILL`，再查询当前任务所属 hart 的就绪队列；没有候选任务时直接
  返回，让 timer trap 恢复被中断的任务。
- 在 `os/src/task/scheduler/mod.rs` 增加统一的 `has_ready_for_hart()` 门面。CFS 以本
  hart 队列是否为空作 O(1) 检查，RR 扫描全局队列，仅把匹配 `home_hart` 的存活任务视为
  候选，保持当前尚未实现跨 hart 迁移的约束。
- `os/src/trap/mod.rs` 的 timer 分支改用新的抢占入口。阻塞、睡眠、显式让出和必须
  退出的路径仍使用 `suspend_current_and_run_next()`，因此不会因优化而跳过必要的
  调度或退出处理。

该检查与入队/取队之间允许存在竞态：检查后新任务到达时最多延迟到下一次 timer；陈旧
队列项可能触发一次多余调度，但不会阻止真实就绪任务被取出，保持了调度正确性。

## 涉及文件

- `os/src/task/mod.rs`
- `os/src/task/scheduler/mod.rs`
- `os/src/task/scheduler/cfs.rs`
- `os/src/task/scheduler/rr.rs`
- `os/src/trap/mod.rs`

## 验证

- `KERNEL_EXTRA_FEATURES=perf make TARGET_ARCH=riscv64` 已完成 RISC-V 与 LoongArch64
  release 构建。
- perf 快照可输出 `scheduler selections` 与 `idle_loops`，用于区分调度选取和空闲循环；
  一次短样本记录为 `selections=194875`、`idle_loops=1088`。
- 同一诊断入口的短样本中，普通内核耗时由约 `707` 降至约 `684`，perf 内核约为
  `679`；该结果受 QEMU、镜像和日志配置影响，未作为正式评分加速比例。
- 尚未完成完整 BuildStorm 446 单元的新旧内核严格 A/B，也未将本优化宣称为完整
  BuildStorm 通过。
