# `rt_sigsuspend` 忙让出导致调度器高频自切换

## 背景

CAgent CPU 单项使用 glibc 工具链启动多个进程，其中一个进程通过
`rt_sigsuspend` 等待定时器信号。性能快照同时记录了请求阶段的大量调度选择。

## 现象

最新 `log.ans` 在 `testcase cagent cpu pass 819` 前后，约 1.2 秒内调度选择从
4096 增至 158928；EXT4 锁等待和读取量没有同步异常增长。`debug.ans` 显示
`SigSuspend` 从约第 1455 行进入、到第 2463 行才因信号返回，期间没有真正的阻塞
状态转换。

## 分析

`sys_rt_sigsuspend` 检查完 pending signal 后调用
`suspend_current_and_run_next()`。该接口将仍可运行的任务重新放入 CFS 队列并切回
调度器；在信号到达前会反复选回同一任务。该路径绕过了已有的 futex/异步等待唤醒
机制，因此调度计数远高于实际 syscall 数量。

## 根因

`rt_sigsuspend` 把“等待信号”实现成了忙让出，而不是把当前任务标记为 `Blocked`。
此外，阻塞前若有信号恰好在调用者检查之后到达，直接置 `Blocked` 可能丢失唤醒。

## 修复

- `sys_rt_sigsuspend` 改用 `block_current_and_run_next()`，由信号投递路径将任务恢复到
  `Ready` 并重新入队。
- `block_current_and_run_next()` 在发布 `Blocked` 前重新检查未屏蔽 pending signal，
  关闭检查与状态转换之间的竞态。
- `sched_yield()` 增加本 hart 就绪竞争者检查；没有其他任务时直接返回。
- 保留 timer future 版本的 `nanosleep`，其阻塞路径同样不再忙让出；无效的
  `sleep(1000)` 输入因 `tv_nsec == 1_000_000_000` 仍按 Linux 规则返回 `EINVAL`。

## 涉及文件

- `os/src/syscall/signal.rs`
- `os/src/task/mod.rs`
- `os/src/syscall/task/schedule.rs`
- `os/src/task/processor.rs`
- `os/src/utils/perf.rs`

## 验证

RISC-V 与 LoongArch64 的 `--release --features warn,scheduler-cfs,perf` 独立 target
编译通过，仅有既有 smoltcp warning。因 QEMU 的 snapshot 临时文件需要宿主
`/var/tmp` 写权限，本轮未重新运行 QEMU；功能与调度计数应以新的 `log.ans` 快照复核。
