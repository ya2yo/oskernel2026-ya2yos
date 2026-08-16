# waitpid 跨进程 ProcessMeta 锁边界修复

## 背景

2026-08-16 的 RISC-V QEMU `oom01` 偶发卡死现场显示，测试在 mlocked OOM 阶段停止，所有 HART 最终进入调度器 idle/WFI。静态检查沿着 LTP wrapper、`waitpid`、子进程退出通知和 pipe 轮询路径继续检查了锁边界。

## 现象

挂死快照没有显示 HART 在内核自旋锁中运行，因此不能仅凭现场确认 `waitpid` 是直接根因。不过 `waitpid`/`waitid` 原实现都在持有父进程 `ProcessMeta` 时读取子进程 `ProcessMeta`，并在同一锁作用域中执行用户内存复制，违反任务模块记录的锁顺序和 uaccess 规则。

## 分析

原实现从父进程 `children` 列表筛选 child 时保持父 `ProcessMeta` 锁，并在筛选、停止/退出状态检查过程中获取 child `ProcessMeta` 锁。找到退出 child 后，父锁还会跨越 `copy_to_user()`，随后才从父列表删除 child 并调用全局 PID 回收。

这会形成父进程锁到子进程锁的跨进程锁链；用户内存复制还可能触发缺页和内存锁路径。并发 waiter 还可能在释放父锁后重复处理同一个 zombie 的 usage 和 PID 回收。

## 根因

根因是 wait-family syscall 将 `ProcessMeta` 锁的生命周期扩大到了 child 状态锁、uaccess 和进程回收路径之外，违反以下约束：

- 多个进程/任务同时访问时应按 pid/tid 顺序加锁，或先复制状态后释放前一个锁；
- 不得持有 `ProcessMeta` 锁访问用户内存；
- 不得持有 `ProcessMeta` 锁进入 PID 表和 procfs 回收路径。

本次 QEMU 快照只证明了 idle/WFI 状态，未证明该锁风险就是那次卡死的直接原因。

## 修复

- `waitpid` 和 `waitid` 先短暂复制父进程的 `Weak<Process>` child 列表并释放父锁。
- child 的 `exit_signal`、停止/继续状态和退出状态在独立的 child metadata 锁作用域内读取。
- `copy_to_user()` 完全在父 `ProcessMeta` 锁释放后执行。
- zombie 回收通过新的短临界区重新确认 child 仍在父列表中；只有成功移除的一方累计 usage、释放 child 并删除全局 PID 映射，避免并发 waiter 重复回收。
- `child_exit_event` 注册使用独立的短锁作用域。

## 涉及文件

- `os/src/syscall/task/wait.rs`

## 验证

- `rustfmt --edition 2021 --check os/src/syscall/task/wait.rs` 通过。
- `git diff --check` 通过。
- `make TARGET_ARCH=riscv64` 通过，并按根 Makefile 同次完成 LoongArch64 release 构建；两架构均成功生成内核。
- 使用维护者已有的 `initproc` 定向入口执行 `timeout 120s make run TARGET_ARCH=riscv64`；`oom01` 四个 OOM 场景为 `passed 4 failed 0 broken 0 skipped 0`，最终正常输出 `shutdown!`。
- 该次运行未复现原始偶发卡死，因此不能据此宣称时序问题已经完全消除。
