# kill05 kill 权限检查与进程组语义

## 背景

LTP `kill05` 用于验证 `kill(2)` 的跨用户信号权限。测试以 root 启动后 fork 出子进程，再分别通过 `setreuid()` 切换到两个不同的普通 uid，随后让其中一个普通用户进程向另一个 uid 拥有的进程发送 `SIGKILL`。

Linux 语义要求：发送者无特权时，只有发送者 real uid 或 effective uid 匹配目标进程 real uid 或 saved set-user-ID，才允许发送信号；`SIGCONT` 还允许同 session 内发送。否则目标存在但无权限时应返回 `EPERM`。

## 现象

修复前最新 `log.ans` 中 musl/glibc 两轮 `kill05` 都失败：

```text
kill05.c:66: TFAIL: kill succeeded unexpectedly
tst_test.c:405: TBROK: Invalid child (...) exit value 1
Summary:
passed   0
failed   1
broken   1
```

测试期望 `kill(pid1, SIGKILL)` 返回 `-1` 且 `errno == EPERM`，但 Ya2yOS 返回成功，导致子进程被错误杀死。

## 分析

`sys_kill()` 原实现只做信号编号检查，然后按 pid 直接调用 `send_signal_to_thread_group()` 或 `send_access_signal()`。`send_signal_to_thread_group()` 只检查目标进程是否存在，并立即向目标线程组加入 pending signal，没有比较发送者和目标进程的 uid。

同时，`sys_kill()` 对部分 pid 取值的处理也偏离 Linux 语义：

- `pid == 0` 应向当前进程所在进程组发送信号，原实现却向当前进程 pid 对应的单个线程组发送信号。
- `pid < -1` 应向进程组 `-pid` 发送信号，原实现同样把 `-pid` 当作单个进程 pid。
- `signo == 0` 应只执行存在性和权限检查，原实现直接返回成功，可能掩盖 `ESRCH/EPERM`。

## 根因

信号投递路径缺少用户态 `kill(2)` 专用的权限检查层，把内核内部信号投递 helper 直接暴露给 syscall 使用。结果是任意普通 uid 进程都能向存在的目标进程发送 `SIGKILL`，违反 Linux `kill(2)` 的权限规则。

## 修复

本次修复保持内核内部信号投递 helper 不做权限检查，用于 `SIGCHLD`、线程组退出等内核路径；另外为用户态 `kill(2)` 增加专用包装：

| 文件 | 修改 |
|------|------|
| `os/src/signal/mod.rs` | 新增 `SignalCred`，从当前任务和目标进程提取 real/effective/saved uid 与 session id；新增 `can_send_signal()`，按 Linux 规则判断 `kill(2)` 权限；新增 `send_user_signal_to_thread_group()`、`send_user_signal_to_process_group()`、`send_user_signal_to_accessible_processes()`，分别处理单进程、进程组和 `kill(-1, sig)` 的存在性/权限/投递结果 |
| `os/src/syscall/signal.rs` | `sys_kill()` 改为 `signo == 0` 使用空 `SigSet` 走检查路径；`pid == 0` 改为当前进程组投递；`pid < -1` 改为进程组投递；目标存在但全部无权限时返回 `EPERM` |

用户可见返回规则同步为：

- 目标不存在：`ESRCH`。
- 至少存在目标但均无权限：`EPERM`。
- 至少一个目标成功投递或 `signo == 0` 检查通过：返回 0。

## 验证

已执行：

```text
rustfmt os/src/signal/mod.rs os/src/syscall/signal.rs
make
```

结果：默认 LoongArch64 构建通过，只有既有 `smoltcp` vendor warning。

维护者提供的最新 `log.ans` 显示 musl 与 glibc 两轮 `kill05` 均通过：

```text
kill05.c:69: TPASS: kill failed with EPERM
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

日志中的 `FAIL LTP CASE kill05 : 10` 和 `RESULT GLIBC LTP SINGLE CASE kill05 : 10` 是 initproc 包装输出；以 LTP 自身的 `TPASS` 与 Summary 为准。
