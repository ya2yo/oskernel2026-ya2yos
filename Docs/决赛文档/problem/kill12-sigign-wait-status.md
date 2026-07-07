# kill12 SIG_IGN 与 wait status 修复

## 背景

LTP `kill12` 验证父进程向子进程发送信号后，`waitpid()` 返回的 status 是否符合 Linux 语义。测试中子进程对 `1..13` 号信号逐个调用 `sigset(sig, SIG_IGN)`，然后等待父进程发送该信号；除不可忽略的 `SIGKILL` 外，子进程应忽略该信号，再由父进程发送 `SIGCHLD` 唤醒 handler，最后 `exit(1)`。

因此测试期望：

- `sig == SIGKILL`：`WTERMSIG(status) == SIGKILL`，`WIFEXITED(status) == 0`。
- 其它信号：`WTERMSIG(status) == 0`，`WIFEXITED(status) == 1`。

## 现象

新的 `log.ans` 中，musl/glibc 单跑 `kill12` 都失败，典型输出为：

```text
wait error: unexpected signal returned, the status of the process is 1
signal error: unexpected exit number returned,the status of the process is 1
...
kill12      1  TFAIL  :  kill12.c:191: Test failed
```

状态值 `1/2/10/12/13/131...` 是 Linux wait status 中的信号终止编码，说明父进程观察到子进程像是被这些信号杀死，而不是正常 `exit(1)`。

## 分析

第一处问题在 `handle_signal()`：`sys_rt_sigaction()` 已经会把 `SIG_IGN` 写入 action table，但 pending signal 分发时只根据 `SigSet::from_sig(signo).default_op()` 决定默认动作，没有优先检查当前 action 是否为显式 `SIG_IGN`。因此 `kill12` 中子进程对 `SIGHUP/SIGINT/...` 设置忽略后，内核仍可能按这些信号的默认终止动作处理。

修复该点后继续验证，临时日志显示 handler 已经是 `0x1` (`SIG_IGN`)，但 `waitpid()` 仍返回信号终止 status。继续追踪发现第二处问题在 `deliver_signal_to_thread_group()`：投递信号时，只要信号默认动作是 terminate/core dump，就提前写入 `ProcessMeta::termination_signal`。这发生在真正分发信号、检查当前 disposition 之前，所以即使后续 `handle_signal()` 正确忽略该信号，进程元数据里仍残留 `termination_signal`。

`sys_waitpid()` 编码 status 时会优先读取 `termination_signal`。于是子进程之后正常 `exit(1)`，父进程仍看到“被信号杀死”的 status。

## 根因

信号投递路径把“信号的默认动作”误当成“最终会执行的动作”，过早记录 `termination_signal`；同时 pending signal 分发路径没有把显式 `SIG_IGN` 作为非 custom action 的优先语义处理。这两点叠加导致被忽略的信号污染 `waitpid()` status。

## 修复

- `handle_signal()` 在非 custom handler 分支中先检查 `sig_action.act.sa_handler == SIG_IGN`，对非 stop 默认动作直接忽略并返回。
- `deliver_signal_to_thread_group()` 不再在投递阶段根据默认动作写入 `termination_signal`。
- 保留实际默认终止路径中的记录：只有 `handle_signal()` 真正执行 `SigOp::Terminate` 或 `SigOp::CoreDump` 时，才设置 `ProcessMeta::termination_signal`，随后退出当前进程。

涉及文件：

- `os/src/signal/pending.rs`
- `os/src/signal/delivery.rs`

## 验证

已执行：

```text
rustfmt os/src/signal/pending.rs os/src/signal/delivery.rs
make
timeout 120s make run > /tmp/kill12-final.log 2>&1
make TARGET_ARCH=riscv64
```

结果：

- 默认 LoongArch64 `make` 通过，仅有既有 `smoltcp` vendor warning。
- LoongArch64 单跑 musl `kill12`：`kill12 1 TPASS : Test passed`，Summary 为 `passed 1 failed 0 broken 0`。
- LoongArch64 单跑 glibc `kill12`：`kill12 1 TPASS : Test passed`，Summary 为 `passed 1 failed 0 broken 0`。
- `make TARGET_ARCH=riscv64` 通过，仅有既有 `smoltcp` vendor warning。
