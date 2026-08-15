# RISC-V QEMU `setuid04` 后 futex requeue 自旋

## 背景

RISC-V QEMU 运行全量 LTP 时，宿主进程长时间保持高 CPU 占用但 guest 输出不再增长。为获取
无法通过 guest gdbstub 连接时的现场，扩展 `scripts/capture_qemu_hang.sh`，使其按
`/proc/<pid>/exe` 同时识别 `qemu-system-riscv64` 和 `qemu-system-loongarch64`，并输出对应
架构的 vCPU 寄存器。

## 现象

现场目录为 `qemu-hang-20260816-022439-pid55664`。`riscv.ans` 最后停在：

```text
RUN LTP CASE setuid04
setuid04.c:49: TPASS: open() returned errno EACCES
```

约 50 分钟后 QEMU 仍保持约 174%--186% CPU。宿主 GDB 显示 8 个 vCPU 中仅 CPU 0 和 CPU 4
处于运行态，其余 CPU 已 halted。RISC-V CPU 0 的 guest PC 为
`0xffffffc08022c8c2`，通过 `riscv64-unknown-elf-addr2line` 定位到
`futex_requeue()` 中 waiter `Arc<TaskControlBlock>` 的释放路径；CPU 4 位于任务 `exec` 的
日志记录路径。

## 分析

`futex_requeue()` 原先持有 `FUTEX_QUEUE_BITMAP`，随后直接调用：

- `wakeup_futex_task()`，获取 `TaskControlBlockInner` 并将任务加入 ready queue；
- `requeue_futex_task()`，获取被阻塞任务的 `TaskControlBlockInner`。

`futex_wake_up_bitset()` 和 `handle_timer()` 也在 futex 队列锁内获取任务锁。该顺序与项目约定
的 `TaskControlBlockInner -> futex/子资源 -> scheduler` 边界冲突；唤醒路径还把调度器入队放在
futex 临界区内。在 all-hart CFS 和线程退出/条件变量并发下，队列操作会长时间占用自旋锁，
表现为 QEMU vCPU 在 futex 路径忙循环而 guest 没有新的测试输出。

## 根因

futex 队列数据锁同时承担了任务状态变更和调度器通知，导致 `FUTEX_QUEUE_BITMAP` 锁的
临界区跨越 `TaskControlBlockInner` 与 ready queue。`FUTEX_REQUEUE` 的 waiter 摘取、唤醒和
重排没有清晰的操作线性化边界，放大了该锁链在多 Hart 下的自旋时间。

## 修复

- 在 `os/src/task/futex.rs` 增加 `FUTEX_OP_LOCK`，串行化 wait、wake、requeue、超时和清理
  操作；队列锁只保护队列容器本身。
- `futex_requeue()` 和 `futex_wake_up_bitset()` 先摘取/整理 waiter，再在释放队列锁后更新
  `TaskControlBlockInner`；唤醒任务暂存到列表，释放 futex 操作锁后再加入 ready queue。
- `futex_wait_bitset()` 先完成任务阻塞状态发布，再获取队列锁登记 waiter，避免
  `FUTEX_QUEUE_BITMAP -> TaskControlBlockInner`。
- `handle_timer()`、信号清理和超时清理统一遵循相同边界。
- 在 `os/src/task/manager.rs` 拆出不触碰调度器的 `mark_futex_task_ready()` 和
  `mark_futex_task_timeout()`，保留原有公共唤醒/超时 API 的行为。

## 涉及文件

- `scripts/capture_qemu_hang.sh`
- `os/src/task/futex.rs`
- `os/src/task/manager.rs`

## 验证

- RISC-V host capture：现场脚本成功识别 `qemu-system-riscv64`，采集 8 个 vCPU 的
  `pc/priv/satp/scause/sepc/stval/mcause/mepc/mtval` 和宿主线程栈。
- `bash -n scripts/capture_qemu_hang.sh`、脚本 `--help`：通过。
- `rustfmt --edition 2021 --check os/src/task/futex.rs os/src/task/manager.rs`：通过。
- `make build-arch TARGET_ARCH=riscv64`：通过；仅有既有依赖和未使用函数 warning。
- `git diff --check`：通过。
- 完整 QEMU/LTP/BuildStorm 尚未以修复后的新内核重跑；现场旧 QEMU 使用的是修复前内核，
  因此本记录不宣称端到端 `setuid04` 后续回归已完成。
