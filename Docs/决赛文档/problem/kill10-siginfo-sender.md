# kill10 SA_SIGINFO 发送者 siginfo 修复

## 背景

LTP `kill10` 是 signal flooding 测试。测试创建 master、manager 和 child 三层进程，manager/child 会反复用 `kill(2)` 互相发送 `SIGUSR1/SIGUSR2`。master 和 manager 使用 `SA_SIGINFO` handler，并依赖 `siginfo_t.si_pid` 判断信号是否来自预期的 manager 或 child。

## 现象

`log.ans` 中单跑 LoongArch64 `ltp-musl kill10` 后未进入 `TPASS/TFAIL` 汇总，而是持续输出：

```text
received unexpected signal 10 from 2
```

`pid 2` 是当前接收信号的 master 进程，说明用户态 handler 看到的 `si_pid` 不是实际发送者 pid。

## 分析

`kill10.c` 的 `ack_ready()`、`ack_done()`、`reply_to_child()` 都会把 `si->si_pid` 放入 checklist 中二分查找。如果 `si_pid` 不是 manager/child 的 pid，测试就会打印 unexpected signal，并继续等待正确来源的确认信号。

内核原实现中，task 只保存 `sig_pending: SigSet` 位图，没有保存 pending signal 对应的 `siginfo_t`。当 `setup_frame()` 为 `SA_SIGINFO` handler 构造用户栈时，直接使用：

```text
SigInfo::new(signo, 0, SI_TKILL, task.pid())
```

这里的 `task.pid()` 是接收者 pid，不是发送者 pid。因此 `kill10` 的 master 收到 manager 的 `SIGUSR1` 时，handler 读取到的 `si_pid` 固定为 master 自己，checklist 匹配失败。

## 根因

pending signal 只有信号编号位图，缺少 Linux `SA_SIGINFO` 可见的发送者信息；signal frame 构造阶段用接收者 pid 临时填充 `si_pid`，破坏了 `kill(2)` 发送信号时 `siginfo_t.si_pid` 应为发送进程 pid 的语义。

## 修复

- `SigInfo` 新增 `new_user()`，用于构造 `kill(2)`、`tkill(2)`、`tgkill(2)` 这类用户态发送信号的 `siginfo_t`，填入发送者 pid、real uid 和 `SI_USER`。
- `TaskControlBlockInner` 新增 `sig_pending_info: [Option<SigInfo>; SIG_MAX_NUM + 1]`，与 `sig_pending` 位图并行保存每个标准信号的一份 siginfo。
- 用户态 `kill/tkill/tgkill` 投递路径记录当前任务作为发送者，并在 signal 还未 pending 时保存对应 siginfo；标准信号仍保持不排队语义。
- `handle_signal()` 消费 pending 信号时同步取出并清理 `sig_pending_info[signo]`，传给 `setup_frame()` 写入用户态 `siginfo_t`。
- `rt_sigtimedwait()` 消费 pending 信号时也复用同一份 siginfo，避免后续等待类测试看到错误发送者信息。
- `execve()`、新建 task/clone 初始化路径清空 `sig_pending_info`。

涉及文件：

- `os/src/signal/types.rs`
- `os/src/signal/delivery.rs`
- `os/src/signal/pending.rs`
- `os/src/signal/frame.rs`
- `os/src/syscall/signal.rs`
- `os/src/task/task/task.rs`

## 验证

已执行：

```text
rustfmt os/src/signal/types.rs os/src/task/task/task.rs os/src/signal/delivery.rs os/src/signal/pending.rs os/src/signal/frame.rs os/src/syscall/signal.rs
make
timeout 120s make run > /tmp/kill10-siginfo.log 2>&1
make TARGET_ARCH=riscv64
```

结果：

- 默认 LoongArch64 `make` 通过，仅有既有 `smoltcp` vendor warning。
- LoongArch64 单跑 musl `kill10`：`kill10 1 TPASS : All 2 pgrps received their signals`，Summary 为 `passed 1 failed 0 broken 0`。
- LoongArch64 单跑 glibc `kill10`：`kill10 1 TPASS : All 2 pgrps received their signals`，Summary 为 `passed 1 failed 0 broken 0`。
- `make TARGET_ARCH=riscv64` 通过，仅有既有 `smoltcp` vendor warning。
