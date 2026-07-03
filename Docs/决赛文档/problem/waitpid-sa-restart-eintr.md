# waitpid SA_RESTART 信号打断语义修复

## 背景

运行 LTP `abort01` 时，LTP harness 会 fork 子进程执行断言，并在父进程侧通过 `waitpid()` 回收子进程状态。测试框架同时安装了带 `SA_RESTART` 的 `SIGUSR1`/`SIGALRM` 等处理函数，用于超时和测试控制。

## 现象

修复前的 `log.ans` 中，`abort01` 子进程向父进程投递 `SIGUSR1` 后，父进程正在阻塞等待子进程退出：

```text
tst_test.c:1654: TBROK: waitpid(3,0x2fff447b78,0) failed: EINTR (4)
```

该 `TBROK` 发生在 LTP 框架 `tst_test.c` 的 waitpid 清理路径，导致用例在断言完成前被判为 broken。

## 分析

`waitpid()` 原实现外层使用：

```text
block_on(interruptible(poll_fn(...)))
```

`add_signal()` 在投递可见信号时，如果目标任务处于 `Blocked`，会调用 `wake_interruptible()` 唤醒等待者。这个路径会设置 `TaskControlBlock::interrupted = true`，供通用 `task::interruptible()` 在下次 poll 前返回 `EINTR`。

但 `waitpid()` 自己已经在内部 `poll_fn` 中实现了 pending signal 语义：

- `SIGCHLD`、显式忽略信号、默认忽略信号：消费后继续等待。
- 自定义 handler 且不带 `SA_RESTART`：返回 `EINTR`。
- 自定义 handler 且带 `SA_RESTART`：应该由 signal frame 路径重启 syscall。

问题在于外层 `interruptible()` 总是先于 `waitpid()` 的内部逻辑检查 `interrupted`。当 `SIGUSR1` 唤醒 `waitpid()` 后，`interruptible()` 直接返回 `EINTR`，内部代码没有机会查看该信号的 `SA_RESTART` 标志。

内核已有 syscall restart 钩子：`setup_frame()` 在信号处理前检查 trap 上下文中的内部 `ERESTART` 返回值；如果 handler 带 `SA_RESTART`，则回退到原 `ecall` 并恢复 syscall 原始参数，使 `sigreturn` 后重新执行 syscall。

因此本次修复不应简单吞掉 `SIGUSR1`，而是让 `waitpid()/waitid()` 在带 `SA_RESTART` 的 handler 场景返回内部 `ERESTART`，交给已有 signal frame 重启机制处理。

## 根因

`waitpid()` / `waitid()` 外层套用了通用 `task::interruptible()`，导致 `wake_interruptible()` 设置的内部唤醒状态被抢先转换成用户可见 `EINTR`。这绕过了 wait 自身对 pending signal、`SIGCHLD`、默认忽略信号和 `SA_RESTART` 的细分语义。

同时，`setup_frame()` 原先比较的是正值 `SysErrNo::ERESTART as usize`，但 syscall 错误返回写入 trap 上下文时使用的是负 errno 表示，即 `-(ERESTART) as usize`。这会使已有 restart 钩子无法识别 syscall 返回的内部 `ERESTART`。

## 修复

修改 `os/src/syscall/task/wait.rs`：

- 移除 `sys_waitpid()` / `sys_waitid()` 外层的 `interruptible()` 包装，改为直接 `block_on(poll_fn(...))`。
- 新增 `wait_pending_signal_errno()` 统一处理 wait 阻塞前的 pending signal。
- 对可忽略信号继续消费并等待。
- 对不带 `SA_RESTART` 的可见信号返回 `EINTR`。
- 对带 `SA_RESTART` 的 handler 返回内部 `ERESTART`，保留信号 pending 状态，让 `trap_return()` 正常建立信号帧并运行用户 handler。

修改 `os/src/signal/mod.rs`：

- `setup_frame()` 改为识别 trap 上下文中的负 errno `-(ERESTART)`。
- 非 `SA_RESTART` 分支写回负 errno `-(EINTR)`，与 syscall 错误返回约定保持一致。

## 涉及文件

- `os/src/syscall/task/wait.rs`
- `os/src/signal/mod.rs`

## 验证

已执行：

```text
make
```

结果：

- 当前默认 RISC-V 配置构建通过。
- 构建过程中仅有既有 warning，例如 `smoltcp` vendor warning 和 `initproc` 未使用函数 warning。

最新 `log.ans` 显示 `abort01` 核心 LTP 结果已通过：

```text
RUN LTP CASE abort01
abort01.c:62: TPASS: abort() dumped core
abort01.c:65: TPASS: abort() raised SIGIOT

Summary:
passed   2
failed   0
broken   0
skipped  0
warnings 0
```

同一份日志中未再出现原始失败：

```text
tst_test.c:1654: TBROK: waitpid(3,0x2fff447b78,0) failed: EINTR (4)
```

日志中仍有测试包装器输出 `FAIL LTP CASE abort01 : 10`，但按照本仓库 LTP 日志判读规则，本次以 LTP 自身 `TPASS`、`broken 0` 和 absence of `TBROK` 为准。

未执行 `TARGET_ARCH=loongarch64` 验证；本次复现和验证基于当前默认 RISC-V 配置与用户提供的最新 `log.ans`。
