# BuildStorm all-hart CFS 重复调度导致的 page fault panic

## 背景

all-hart CFS 使用一个由所有 Hart 竞争的共享就绪堆。任务的 `on_rq` 标志和
`TaskStatus` 必须在取任务、唤醒和重新入队之间保持一致，否则同一个 TCB 可能
被多个 Hart 同时恢复。

## 现象

`server.ans` 在 `buildstorm` 阶段首先报告 `Cause: Exception(LoadPageFault)`，
`stval=0x2a`；随后多个 Hart 报告 `spin::once::Once panicked`。`client.ans` 的
回溯落在 `Arc<DetachedMountFd>::clone`、任务表访问、`mmap_write_page_fault`
和 `current_trap_cx`，并出现异常的当前任务指针。这些是调度同一 TCB 造成内核
栈、页表或共享对象状态损坏后的连锁故障，不是 `spin::Once` 初始化本身的首发
错误。

## 分析

旧的 `fetch_task` 从共享堆弹出条目后先清除 `on_rq`，但任务仍保持 `Ready`，
直到 `run_tasks` 取得返回值后才发布 `Running`。在这个窗口中，唤醒/重新入队
路径可以观察到“未入队且 Ready”的 TCB，创建第二个有效堆项；另一个 Hart
随后能够再次选中同一任务。共享地址空间下，这会并发使用同一个用户页表和任务
内核上下文，最终表现为随机 page fault 和后续 panic。

## 根因

队列占用状态与任务运行状态不是同一个临界区内的状态机转换：`on_rq=false`、
`TaskStatus::Ready` 和“已被某个 Hart 预留”同时可见，导致 CFS 的去重保证失效。

## 修复

- `add_task` 先持有共享队列锁，再检查任务是否仍为 `Ready`，并在同一锁协议下
  执行 `on_rq` 去重，拒绝非就绪任务的 stale 入队。
- `fetch_task` 在共享队列锁和 TCB 锁保护下，将可执行任务从 `Ready` 预留为
  `Running`，然后清除 `on_rq`，使其他 Hart 看不到可重复选择的状态窗口。
- 取任务后若 affinity 已变化，调度循环先把预留状态恢复为 `Ready`，再重新入队。

## 涉及文件

- `os/src/task/scheduler/cfs.rs`
- `os/src/task/processor.rs`

## 验证

- `make TARGET_ARCH=riscv64`：通过；默认构建流程同时完成 LoongArch64 构建。
- `git diff --check`：通过。
- 使用 `/tmp/ya2yos-cfs-fix.qcow2` 隔离 overlay 运行 RISC-V QEMU 240 秒：8 个
  Hart 均启动，`sigaltstack regression: PASS`、`rseq regression: PASS`，进入
  `buildstorm` 并输出 `BUILDSTORM_TOOLCHAIN ok`；日志无 `panic`、`LoadPageFault`、
  `StorePageFault`、`TFAIL` 或 `TBROK`。测试在完整 BuildStorm 结束前因超时停止，
  未据此宣称 446/446 完整通过。
- 正式 `make run` 未使用，因为维护者已有 QEMU 进程持有正式镜像写锁；未终止该
  进程，也未修改正式镜像。
