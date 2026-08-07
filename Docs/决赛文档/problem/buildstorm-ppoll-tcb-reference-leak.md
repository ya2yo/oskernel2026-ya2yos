# BuildStorm ppoll 退出路径的 TCB 强引用泄露

## 背景

Ya2yOS 的任务退出路径不会展开当前内核栈。`exit_current_and_run_next()` 完成任务资源
清理后调用 `abandon()` 切回调度器，由调度器从全局 TID 表移除最后的生命周期引用。
因此，可能跨越阻塞点并被致命信号中止的 syscall 不能依赖栈上对象的 `Drop` 来释放
`Arc<TaskControlBlock>`。

## 现象

最新 `server.ans` 在 BuildStorm 测试组启动后连续报告：

```text
tid 15 exits with extra TCB refs, strong_count = 4
tid 13 exits with extra TCB refs, strong_count = 4
tid 14 exits with extra TCB refs, strong_count = 4
```

告警发生在 `rustc --version`、`cargo --version` 和 `BUILDSTORM_TOOLCHAIN ok` 之前。
此前范围化 `MemorySet` 帧保留的运行日志也出现过 `strong_count = 4/5`，说明它不是
`FrameTracker` 范围收集本身的计数。

## 分析

本轮 MM 修改中的 `retained_frames: Vec<Arc<FrameTracker>>` 在 remote TLB ACK 后已有
显式 `drop(retained_frames)`，并且四个 MM 修改文件没有新增
`Arc<TaskControlBlock>`。日志报出的对象类型也是 TCB，而不是物理帧。

继续审计所有长期持有 TCB 的位置后，发现 `sys_ppoll()` 的
`PpollSigMaskGuard` 保存了当前任务的强引用：

```text
PpollSigMaskGuard -> Arc<TaskControlBlock>
```

guard 用于在 `ppoll` 返回时恢复临时信号掩码。普通返回时其 `Drop` 会执行，因此不泄露；
但任务阻塞在 `ppoll` 中并收到致命信号时，信号处理会直接进入
`exit_current_and_run_next()`。该退出路径通过 `abandon()` 丢弃整段内核调用栈，不运行
栈上 guard 的析构函数，TCB 强引用因而永久少一次 `drop`，退出检查稳定多出一份引用。

## 根因

`PpollSigMaskGuard` 错误地把“正常返回时临时访问 TCB”的需求实现成跨阻塞点拥有 TCB。
在不展开栈的内核退出模型下，拥有型 guard 与任务本身形成了无法由退出路径清理的生命周期
依赖。问题不是缺少一条普通控制流上的 `drop()`，而是致命退出根本不会执行该控制流。

## 修复

将 `PpollSigMaskGuard.task` 从 `Arc<TaskControlBlock>` 改为
`Weak<TaskControlBlock>`：

- 创建 guard 时完成临时信号掩码替换，并只保存 `Arc::downgrade()` 的弱引用。
- `ppoll` 正常返回时，`Drop` 升级弱引用并恢复旧掩码，保持原有 syscall 语义。
- 致命信号退出时，即使栈不展开，弱引用也不会增加 TCB `strong_count`，不再阻止任务
  和内核栈回收；正在退出的任务无需恢复用户态信号掩码。

涉及文件：`os/src/syscall/io_mpx/poll.rs`。

## 验证

- `rustfmt --check os/src/syscall/io_mpx/poll.rs` 通过。
- `git diff --check` 通过。
- `make TARGET_ARCH=riscv64` 完成 RISC-V 和 LoongArch64 release 构建。
- 修复后的 RISC-V snapshot 运行通过 `BUILDSTORM_TOOLCHAIN ok`、
  `BUILDSTORM_MINIBUILD ok`，推进到 `444/446: axbuild`；原启动阶段的三条
  `extra TCB refs` 未再出现，也没有 panic。

运行由 180 秒外层 timeout 终止，未完成完整 BuildStorm，因此这里只确认 TCB 引用告警
消失和前置路径正常，不报告端到端 `446/446` 通过。
