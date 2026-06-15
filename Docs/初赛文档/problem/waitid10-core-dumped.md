# waitid10: core dump 信号终止状态

## 背景

LTP `waitid10` 会让子进程触发 `SIGFPE`，并将 `RLIMIT_CORE` 提高到非 0，然后通过 `waitid(P_ALL, 0, infop, WEXITED)` 检查父进程拿到的 `siginfo_t`。

该用例期望：

- `si_status == SIGFPE`
- `si_signo == SIGCHLD`
- `si_code == CLD_DUMPED`

## 现象

`log.ans` 中 `waitid10` 失败：

```text
waitid10.c:36: TPASS: waitid(P_ALL, 0, infop, WEXITED) passed
waitid10.c:37: TPASS: infop->si_pid == pidchild (4)
waitid10.c:38: TFAIL: infop->si_status (136) != SIGFPE (8)
waitid10.c:39: TPASS: infop->si_signo == SIGCHLD (17)
waitid10.c:42: TFAIL: infop->si_code (1) != CLD_DUMPED (3)
```

其中 `136 == 128 + SIGFPE`。

## 分析

Ya2yOS 的默认信号终止路径会调用：

```text
exit_current_and_run_next(128 + signo)
```

这能兼容 shell 风格退出码，但会丢失“该进程是被哪个信号终止、是否属于 core dump 默认动作”的结构化信息。

`sys_waitid()` 在处理退出事件时只读取 `exit_code()`，并固定填充：

```text
si_code = CLD_EXITED
si_status = exit_code
```

因此子进程因 `SIGFPE` 默认 core dump 动作终止时，父进程看到的是普通退出事件 `CLD_EXITED` 和状态 `136`，不符合 Linux `waitid()` 的 `SIGCHLD` 语义。

## 根因

进程元数据没有保存默认信号动作导致的终止原因，`waitid()` 无法区分：

- 普通 `exit(code)`
- 默认终止信号 `CLD_KILLED`
- 默认 core dump 信号 `CLD_DUMPED`

## 修复

在 `ProcessMeta` 中增加：

```rust
termination_signal: Option<(usize, bool)>
```

含义为 `(signo, dumped_core)`。

默认信号处理遇到 `SigOp::Terminate | SigOp::CoreDump` 时，在退出前记录终止信号和是否 core dump。`sys_waitid()` 处理退出事件时，如果发现 `termination_signal`：

- `si_status` 填原始信号号
- `dumped_core == true` 时 `si_code = CLD_DUMPED`
- 否则 `si_code = CLD_KILLED`

普通 `exit()` 路径继续返回 `CLD_EXITED + exit_code`。

该修复只修正 waitid 可见的状态语义，不实现真实 core dump 文件生成。

## 涉及文件

- `os/src/task/process/process.rs`
- `os/src/signal/mod.rs`
- `os/src/syscall/task/wait.rs`

## 验证

已执行：

```text
make
timeout 90s make run
```

结果：

```text
waitid10.c:36: TPASS: waitid(P_ALL, 0, infop, WEXITED) passed
waitid10.c:37: TPASS: infop->si_pid == pidchild (4)
waitid10.c:38: TPASS: infop->si_status == SIGFPE (8)
waitid10.c:39: TPASS: infop->si_signo == SIGCHLD (17)
waitid10.c:42: TPASS: infop->si_code == CLD_DUMPED (3)

Summary:
passed   5
failed   0
broken   0
skipped  0
warnings 0
```
