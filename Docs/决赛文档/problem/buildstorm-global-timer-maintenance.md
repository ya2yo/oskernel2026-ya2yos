# BuildStorm 全局 timer maintenance 重复执行

## 背景

BuildStorm 的 Rust/Cargo 阶段会同时创建大量超时等待、future timer 和阻塞任务。它们使用的
timer wheel、futex timeout heap 与任务表均为全局状态；每个 Hart 对同一 10 ms 时间桶做相同
维护不会提高计时精度，只会放大共享锁争用。

## 现象

早期 `client.ans` 的 LoongArch64 GDB 快照中，Hart 0、1、2、5、6 同时停在
`task::future::time::check_timer_events() -> TIMER_RUNTIME.lock()` 的自旋路径。其余 Hart
位于 idle 或用户态，说明这不是单一 CFS 任务长时间占用 CPU，而是多个 timer interrupt
与调度循环并发争抢同一全局 timer runtime。

源码中每个 Hart 的 timer interrupt 都调用 `check_timer_events()` 和
`check_futex_timer()`；`processor::run_tasks()` 也会在每个 Hart 的新 10 ms bucket 进行同样
的维护。`check_blocked_task_timers()` 更会由每个 Hart 遍历完整 `TID_TO_TASK`，最后才用
`scheduled_hart` 过滤候选任务。活跃 Hart 增加时，这形成近似“Hart 数乘任务数”的重复扫描。

## 根因

全局 timer/futex 数据结构以共享单调时钟判定到期，但旧实现把维护责任错误地复制给每个
Hart。timer interrupt 和 scheduler 补扫都可能在同一时间桶重复拿锁、扫描任务和投递同一类
到期事件。

## 修复

- 在 `timer/mod.rs` 增加 `LAST_GLOBAL_TIMER_MAINTENANCE_TICK` 与
  `claim_global_timer_maintenance()`。它以 CAS 保证一个 10 ms bucket 仅有一个 Hart 获得
  全局维护资格。
- trap 的 timer interrupt 和 `processor::run_tasks()` 均仅由 winner 执行
  `check_timer_events()`、`check_blocked_task_timers()` 与 `check_futex_timer()`。
- 每个 Hart 对当前 Running task 的 `deliver_itimer_signal()` 保持在 timer interrupt 原路径，
  因而没有把线程正在执行的 `ITIMER_REAL` 投递延后到其他 Hart。
- `check_blocked_task_timers()` 由唯一 owner 扫描所有 blocked task，移除原来只能扫描到本
  Hart placement 的过滤。共享 timer runtime 的单次扫描即可覆盖所有任务。

future timer wheel 和 futex timeout heap 都使用全局单调时钟，一个 10 ms bucket 一次检查
与原先的维护精度相同。任务状态、timer 和 ready queue 的现有锁仍处理到期与并发唤醒之间的
竞争；本修改不宣称提高 timer 精度。

## 涉及文件

- `os/src/timer/mod.rs`
- `os/src/trap/mod.rs`
- `os/src/task/processor.rs`
- `os/src/task/manager.rs`
- `Docs/决赛文档/problem/buildstorm-global-timer-maintenance.md`

## 验证

已通过：

```text
rustfmt --edition 2021 --check os/src/timer/mod.rs os/src/task/processor.rs os/src/task/manager.rs os/src/trap/mod.rs
git diff --check
make TARGET_ARCH=riscv64
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
```

两次使用 final LoongArch64 原始镜像的独立 qcow2 overlay、12 Hart perf 内核的冒烟均完成
十项 CAgent 并输出 `OS COMP TEST GROUP END cagent`、`BUILDSTORM_TOOLCHAIN ok`；未见
`panic`、`TFAIL` 或 `TBROK`。两次 CAgent 耗时受 QEMU 宿主抖动影响明显，约为 1.19--2.57
秒和 2.87--7.10 秒，不能据此宣称稳定百分比加速。运行环境在 toolchain 后结束，未得到
完整 BuildStorm 或 `shutdown!`，因此完整长程回归仍待执行。
