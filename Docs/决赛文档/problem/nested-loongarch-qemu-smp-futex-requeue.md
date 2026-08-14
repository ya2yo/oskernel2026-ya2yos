# 嵌套 LoongArch64 QEMU 多核 futex requeue 丢 waiter

## 背景

外层 Ya2yOS 以 `TARGET_ARCH=loongarch64 SMP=12` 启动时，`initproc` 会运行
`/opt/qemu-la64/qemu-system-loongarch64`。内层 guest 应经 EFI 启动 ArceOS 并输出
`Hello, world!`、`shutdown!`。

单核运行可以完成该路径，多核运行则会在内层 EDK2 或 ArceOS ACPI 初始化阶段停止。
外层日志末尾重复出现 `ppoll`、`ClockGettime`、`Read`、`Write` 和 futex 调用；host GDB
快照中的外层 vCPU 线程处于 futex 等待，主循环处于 `ppoll`。

## 归因

`ppoll` 的可读 fd 是 QEMU 使用的 eventfd，不是 timerfd；且 `ppoll(timeout=1s)` 的内核
timeout 在多个 hart 上均可到期，排除时钟推进或 timerfd 路径。

临时将新线程的默认 affinity 固定到 `home_hart` 后，外层仍以 12 hart 启动，内层 guest
稳定输出 `Hello, world!`、`shutdown!`。该对照只说明触发条件是多线程跨 hart 并发，不是最终
方案，随后已完整撤销。

继续审计 futex 状态机发现，每个 `FUTEX_WAIT` 都有唯一 `futex_key`，但
`FUTEX_WAKE` 与 `FUTEX_REQUEUE` 从队列弹出 waiter 后没有验证任务仍处于该代 `Blocked` 状态：

- 过期 waiter 仍会消耗 `FUTEX_WAKE(1)` 的唤醒配额，并可能清空线程下一轮 wait 的记录；
- `FUTEX_REQUEUE` 只保留可重排数以内的 waiter，其余 waiter 被直接丢弃；
- 被移动的 waiter 没有同步更新任务的 `futex_pa`，后续 timeout 或信号清理仍在旧队列查找。

QEMU 的 eventfd 与 pthread 条件变量会频繁使用 wake/requeue；在多个 hart 并发调度时，上述
错误会丢失唯一可见的唤醒，最终留下所有外层 hart idle 而 QEMU 线程互相等待。

## 修复

- `wakeup_futex_task()` 仅在 `task_status == Blocked` 且 `futex_key` 匹配时完成
  `Blocked -> Ready`，并以布尔返回值决定是否消耗 wake 配额。
- timeout 使用同一 generation 检查，以保证普通 wake 已获胜时不会把结果改写为超时。
- `FUTEX_REQUEUE` 仅移动仍匹配的 waiter，同步更新其 `futex_pa`；超过重排上限的 waiter
  原样留在旧队列，系统调用返回实际唤醒数加实际重排数。
- 已失效 weak waiter 只清理，不再触发 panic。

## 涉及文件

- `os/src/task/futex.rs`
- `os/src/task/manager.rs`

## 验证

- `make build-arch TARGET_ARCH=loongarch64` 通过。
- `make log TARGET_ARCH=loongarch64` 通过。
- 在伪终端中执行 `make run TARGET_ARCH=loongarch64 SMP=12`，使用默认 all-hart affinity。
  内层日志依次进入 EFI、ArceOS ACPI 初始化，并输出：

  ```text
  Hello, world!
  shutdown!
  ```

- 运行日志未包含 `panic`、`TFAIL` 或 `TBROK`。

本轮未修改 `os/src/arch/loongarch64/qemu/cpu.rs`、SMP 数量、超时或测试脚本配置。
