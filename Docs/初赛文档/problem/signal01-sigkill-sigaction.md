# signal01: SIGKILL/SIGSTOP sigaction 与 pause/ppoll 等待

## 背景

glibc LTP `signal01` 验证 `signal(2)` 对不可捕获信号的处理语义。测试会反复 fork 子进程，前 3 组子进程调用 `signal(SIGKILL, handler)` 后应立即得到 `EINVAL`；后 3 组子进程调用同一路径后进入 `pause()`，父进程随后 `kill(child, SIGKILL)` 并通过 `wait4` 检查子进程是否确实被 `SIGKILL` 终止。

`SIGKILL` 和 `SIGSTOP` 是 Linux/POSIX 语义中的不可捕获、不可忽略信号。用户态通过 `signal()` 或 `rt_sigaction()` 提供新的 action 时，内核必须拒绝该修改并返回 `EINVAL`。

## 现象

`log.ans` 中 `signal01` 单跑能推进到 Summary，但存在失败项：

```text
signal01.c:56: TFAIL: (long)signal(SIGKILL, tc->sighandler) invalid retval ...: SUCCESS (0)
signal01.c:72: TFAIL: Child not killed by signal
```

调用链显示失败子进程进入 `sys_rt_sigaction(SIGKILL, act, old_act)` 后，内核返回成功：

```text
[sys_rt_sigaction] signo is 9, sig is SIGKILL, act is SigAction { sa_handler: 0, ... }
[syscall ret --- OK] SigAction ret = 0
```

进一步对比 `make` 和 `make log` 生成的 kernel：顶层 `make` 使用 `KERNEL_OUTPUT_LOG_LEVEL=warn`，`make log` 使用 `KERNEL_OUTPUT_LOG_LEVEL=debug`。`os/src/logger.rs` 中这些 feature 只影响日志过滤级别，不应改变内核语义；实际差异来自 debug 日志拖慢执行，改变了 `signal01` 父子进程调度时序。warn kernel 运行更快，后 3 组 `pause()`/`SIGKILL` 路径更容易进入卡死。

## 分析

第一处问题在 `os/src/syscall/signal.rs`。原有逻辑只拒绝 `SIGKILL/SIGSTOP` 的非零 handler：

```rust
if new_act.sa_handler != 0 {
    return Err(SysErrNo::EINVAL);
}
```

这意味着用户态传入 `SIG_DFL` 时，内核会把 `SIGKILL` 的 action 当作合法修改写入信号表并返回 0。glibc 的 `signal()` wrapper 因此看到调用成功，LTP 将其判定为 `TFAIL`。

实际语义不是“不能捕获或忽略，但可以重设默认”，而是只要尝试为 `SIGKILL/SIGSTOP` 安装新的 action，就应失败。`act == NULL` 查询旧 action 可以保留。

第二处问题在 `os/src/syscall/io_mpx/poll.rs`。glibc 的 `pause()` 路径会进入等价于 `ppoll(NULL, 0, NULL, NULL)` 的空 fd 集合无限等待。原 `sys_ppoll()` 对 `fds_ptr == NULL` 无条件返回 `EINVAL`，并且等待循环没有在 pending signal 到来时返回 `EINTR`。因此父进程发出 `SIGKILL` 后，阻塞在 `pause()`/`ppoll` 的子进程无法按 Linux 语义从 interruptible wait 中被信号打断，批量运行或 warn 构建时就表现为等待卡死。

## 根因

- `sys_rt_sigaction()` 对不可捕获信号的校验过窄，只禁止了非默认 handler，没有禁止 `SIG_DFL` 重设，导致 `signal(SIGKILL, SIG_DFL)` 错误返回成功。
- `sys_ppoll()` 不支持合法的 `nfds == 0 && fds_ptr == NULL` 等待形式，也没有在等待过程中检查 pending signal 并返回 `EINTR`，导致 `pause()` 不能被 `SIGKILL` 正确打断。
- `make log` 的 debug 日志只是在时序上掩盖了问题；`make` 和 `make log` 的功能差异不是根因。

## 修复

修改 `os/src/syscall/signal.rs`：

- 当 `signo` 为 `SIGKILL` 或 `SIGSTOP` 且 `act != NULL` 时，立即返回 `EINVAL`。
- 不再读取用户传入的 `SigAction` 后按 `sa_handler` 分支判断。
- 保留 `act == NULL` 查询旧 action 的路径。

修改 `os/src/syscall/io_mpx/poll.rs`：

- 允许 `nfds == 0 && fds_ptr == NULL`，仅在 `fds_ptr == NULL && nfds != 0` 时返回 `EINVAL`。
- `nfds == 0` 时不再对空 fd 数组执行 `copy_from_user()`。
- `ppoll` 等待循环中检查当前任务 pending signal；发现信号后返回 `EINTR`，让 `pause()`/signal 路径继续推进。
- 移除等待循环中不必要的 `task.inner_lock()`，避免检查信号时重入同一任务锁并触发 `fail to get task inner` panic。

修复后日志中的关键路径变为：

```text
[sys_rt_sigaction] attempt to change SIGKILL/SIGSTOP action
[syscall ret --- Err] SigAction ret = Invalid argument
signal01.c:56: TPASS: (long)signal(SIGKILL, tc->sighandler) : EINVAL (22)
```

## 涉及文件

- `os/src/syscall/signal.rs`
- `os/src/syscall/io_mpx/poll.rs`
- `Docs/初赛文档/开发日志.md`
- `Docs/初赛文档/problem/README.md`
- `Docs/初赛文档/problem/signal01-sigkill-sigaction.md`
- `Docs/初赛文档/ai.log`
- `Docs/初赛文档/AI_INTERACTION.md`

## 验证

已执行：

```text
make
make log
timeout 120s make run
```

结果：

```text
Summary:
passed   6
failed   0
broken   0
skipped  0
warnings 0

shutdown!
[syscall begin] MachineShutdown
```

`make log` 生成的 debug kernel 和 `make` 生成的 warn kernel 单跑 `signal01` 均完成 6 项 TPASS，并正常 `shutdown!`。`rg -a -n "TFAIL|TBROK" log.ans` 未发现 `signal01` 的失败项。
