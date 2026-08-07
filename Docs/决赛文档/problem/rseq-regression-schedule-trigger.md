# rseq 回归的实际调度触发

## 背景

`8ff0a49b` 将 rseq 用户态回写改为事件驱动：注册、实际上下文切出、迁核、exec
和信号路径才会设置 `rseq_pending`；没有事件的普通 syscall 返回不再访问用户 rseq
区域。`server.ans` 在 initproc 启动阶段先通过 `sigaltstack regression`，随后报出：

```text
rseq regression: FAIL (scheduled return cleanup)
```

## 现象

旧的 `rseq_regression` 先把范围外的 `RSEQ_CS_OUTSIDE` 写入 `rseq_cs`，验证重复
`rseq()` syscall 不会清理它，然后调用 `sleep(1)`，并断言返回后 descriptor 已清零。
将等待时间提高到 20ms 后失败仍可复现，因此不能归因为单纯的 1ms 精度问题。

## 分析

`sleep_until()` 的 Future 先注册计时器，再由 `block_on()` 调用
`MyWaker::block_current_if_not_woken()` 发布 `Blocked` 状态。在二者之间，任意 Hart
都可以处理到期计时器。若 waker 发现任务仍是 `Running`，它只设置 `woke`；随后
`block_current_if_not_woken()` 消费该标志并直接返回，不会调用 `schedule()`。

临时诊断确认失败路径没有打印调度器的 `rseq_pending` 标记，返回用户态时也观察到
`pending=false`。因此该次 `nanosleep` 实际没有上下文切出，按事件驱动 rseq 的设计不应
清理 `rseq_cs`；内核实现本身没有违反“无调度 syscall 不是 rseq event”的语义。

## 根因

回归探针把一次可能同步完成的 `sleep()` 当成必然发生的上下文切换。该假设与
`block_on()` 的唤醒竞态不兼容，造成 `server.ans` 中的假失败。

## 修复

- 删除 `sleep(1)` 触发方式。
- 改用 `vfork()`：父任务在子进程 `exit(0)` 前必然不能返回用户态，因此会经过实际
  调度切出并设置 `rseq_pending`。
- vfork 子进程继承共享地址空间但拥有独立、未注册的 rseq 状态；子分支只调用
  `exit(0)`，不修改父进程的 rseq ABI 区域。

涉及文件：`user/src/bin/initproc/rseq_regression.rs`。

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过；只有 vendored `smoltcp` 的既有
  unused warning。
- `timeout 45s make TARGET_ARCH=riscv64 run`：输出 `sigaltstack regression: PASS` 和
  `rseq regression: PASS`，随后进入 `BUILDSTORM_TOOLCHAIN ok`、
  `BUILDSTORM_MINIBUILD ok`。外层时间窗结束在后续 BuildStorm 预构建阶段，未完成
  446/446。
- 未运行 LoongArch64 QEMU；该架构的同一用户态回归入口仍待运行时复测。
