# BusyBox fork/exec/exit 并发路径卡死与地址空间回收修复

## 背景

`user/src/bin/initproc.rs` 的临时回归入口依次执行 musl 和 glibc 的
`busybox_testcode.sh`。BusyBox 脚本会高频创建短生命周期子进程，并覆盖
`fork()`、`execve()`、`wait()`、futex 超时/信号唤醒与后台任务退出等共享任务路径。
RISC-V 默认运行于两个 hart，父子进程和唤醒者可以真正并发。

此前普通 `fork/exit` 的锁序死锁已被修复，但 BusyBox 压力仍会卡死或在后续用户态分配
阶段发生破坏，说明 `exec`、futex 和就绪队列仍有独立的生命周期竞态。

## 现象

修复前，BusyBox 回归不能稳定结束：脚本中的短命令或后台任务执行后 QEMU 停滞，无法打印
测试组结束标记和 `shutdown!`。卡点会随调度时序在 `fork/exec/exit`、阻塞唤醒和后续分配
阶段漂移，并不固定于某一个 BusyBox applet。

当前 `log.ans` 已出现：

```text
#### OS COMP TEST GROUP END busybox-musl ####
#### OS COMP TEST GROUP END busybox-glibc ####
shutdown!
```

这证明两组脚本均穿过原先的卡死区间。日志仍有 `hwclock`、`mv/rmdir` 和后台
`sleep/kill` 的用例失败，glibc 组还记录了一次 `malloc` assertion；它们没有阻止测试组
收尾，且本次未将其误记为已经修复的 BusyBox 语义问题。

## 分析

### futex 唤醒与陈旧就绪项

futex 超时和信号可在 blocked task 已被另一条路径唤醒、重新调度之后交错。旧的
`wakeup_futex_task()` 无条件将任务设为 `Ready` 并再次入队；当对象已处于 `Running` 或
`Zombie` 时，会留下陈旧 runqueue entry。后续调度器仍可能取出该项，最坏时会在内核栈或
地址空间释放后继续执行。

CFS 的 `on_rq` 位只能抑制同一时刻的重复入队，不能使已经入队的旧 weak entry 自动失效；
RR 的 TID 集合也有相同边界。因此取队时还必须确认任务仍为 `Ready`。

### exec/调度锁序与地址空间切换

`exec()` 的 vfork 父任务唤醒需要读取父进程 task 列表并检查父 task 状态。旧路径在当前
task inner 锁域内访问父进程元数据，容易与调度和父子进程退出路径形成
`TaskControlBlockInner -> ProcessMeta` 的反向锁链。调度路径也在 task inner 锁前调用了
封装的退出状态查询，使锁序不够明确。

此外，普通 `exec` 在替换 `Process` 保存的 `MemorySet` 时可释放旧地址空间；线程组最后
退出时也会主动回收页表和数据页。若替换或回收后才切换地址空间，当前 hart 的 `satp`
仍可能指向已被 frame allocator 回收的根页，后续内核分配或返回路径便会在错误地址翻译
状态中运行，表现为随机 fault、allocator 破坏或停滞。

## 根因

BusyBox 暴露的是内核任务生命周期路径的组合问题，而非 BusyBox applet 本身：

- futex 重复唤醒可将 `Running`/`Zombie` task 重新入队，留下陈旧调度项；
- CFS 与 RR 取队时未隔离既有的非 `Ready` 项；
- `exec`/调度路径存在与既定锁序不一致的锁域；
- `exec` 替换或 group exit 回收地址空间前，当前 hart 仍可能使用旧用户页表。

双 hart 并发和 BusyBox 高频短进程扩大了窗口，因此故障表现为时序相关卡死，而非单一
applet 的确定性返回码错误。

## 修复

- `wakeup_futex_task()` 只允许 `Blocked -> Ready` 转换入队；无论状态为何都清理 futex
  key/物理地址，避免超时和信号残留状态。
- CFS 与 RR 的 `fetch_task()` 在移出队列后检查 `TaskStatus`，非 `Ready` 项直接丢弃；
  CFS 同时清除该实体的 `on_rq` 位。
- `exec()` 先快照父进程 task 列表、释放 `ProcessMeta` 后再检查 vfork 父任务；实际
  `add_task()` 在 task inner 锁之外执行。进程名更新也移到 task inner 解锁后。
- `suspend_current_and_run_next()` 先读取 `group_exit_code` 再获取 task inner，保持
  `ProcessMeta -> TaskControlBlockInner` 顺序。
- `exec()` 在替换进程 `MemorySet` 前激活新页表；最后一个线程退出时先
  `activate_kernel_space()`，再回收原地址空间。`exec()` 同时重置旧映像中的
  `robust_list` 用户指针。

## 涉及文件

- `os/src/task/manager.rs`
- `os/src/task/mod.rs`
- `os/src/task/scheduler/cfs.rs`
- `os/src/task/scheduler/rr.rs`
- `os/src/task/task/task.rs`
- `user/src/bin/initproc.rs`（仅将回归入口收敛到 BusyBox musl/glibc）

## 验证

- 审阅仓库根目录 `log.ans`：musl 与 glibc 两组均打印 `OS COMP TEST GROUP END`，最终
  打印 `shutdown!`，未在 BusyBox 回归中卡死。
- `git diff --check`：通过。
- 本次文档更新未重新执行内核构建或 QEMU；上述运行结论来自当前工作区已有的 BusyBox
  回归日志。

## 剩余风险

- 当前日志不是 BusyBox 全量成功证明：`hwclock` 受 RTC ioctl 限制，`mv/rmdir` 和后台
  `sleep/kill` 仍失败；glibc 后台用例附近出现的 `malloc` assertion 也需要独立复现。
- 陈旧 entry 的取队过滤是防御层；后续新增唤醒源时仍应保持“只有 `Blocked -> Ready`
  路径负责入队”的单一所有权。
- 尚未在本轮工作区状态下重新取得 LoongArch64 BusyBox 回归样本，不能据此声称双架构
  BusyBox 行为均已验证。
