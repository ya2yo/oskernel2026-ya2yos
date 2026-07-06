# fcntl31 F_SETOWN_EX 与异步 I/O 信号修复

## 背景

LTP `fcntl31` 验证 `fcntl(2)` 的异步 I/O owner 和信号接口：

- `F_GETOWN` / `F_SETOWN`
- `F_GETOWN_EX` / `F_SETOWN_EX`
- `F_GETSIG` / `F_SETSIG`

测试对象是匿名 pipe。测试会在读端 fd 上设置 `O_ASYNC`、设置 owner 和 `SIGUSR1`，再让子进程向 pipe 写入数据。父进程阻塞在 `sigtimedwait()`，期望收到 `SIGUSR1`，而不是默认 `SIGIO`。

## 现象

旧日志中 musl 和 glibc 两轮都失败在 setup 阶段：

```text
fcntl31     1  TFAIL  :  fcntl31.c:129: fcntl get original f_owner_ex info failed: TEST_ERRNO=EINVAL(22): Invalid argument
Summary:
passed   0
failed   1
broken   0
```

也就是 `fcntl(test_fd, F_GETOWN_EX, &orig_own_ex)` 返回了 `EINVAL`。

## 分析

`os/src/syscall/fs/fd_ops.rs` 中原实现对 `F_SETOWN_EX | F_GETOWN_EX` 统一返回 `EINVAL`，因此测试在读取初始 `struct f_owner_ex` 时必然失败。

继续对照 `fcntl31.c` 后发现，只让 `F_GETOWN_EX` 成功还不够。测试后续会执行：

1. `F_SETOWN` 或 `F_SETOWN_EX` 设置 owner 为当前进程、线程或进程组。
2. `F_SETSIG(SIGUSR1)` 设置异步 I/O 通知信号。
3. 子进程写 pipe。
4. 父进程期望收到 `SIGUSR1`。

因此内核需要保存 fd 关联文件对象的 async owner/signal，并在 pipe 从不可读变为可读时向对应 owner 投递信号。

## 根因

根因是 fcntl async I/O owner 语义缺失：

- `F_GETOWN_EX/F_SETOWN_EX` 直接返回 `EINVAL`。
- `F_SETOWN/F_GETOWN/F_SETSIG/F_GETSIG` 没有保存实际状态。
- pipe 写入数据后只唤醒 reader，没有根据 `O_ASYNC`/owner/signal 投递异步 I/O 信号。
- `send_signal_to_process_group()` 仍是 `todo!()`，无法支持 `F_OWNER_PGRP`。

## 修复

本次补齐最小可见语义：

- 在 `File` trait 中加入 `FasyncOwner` 状态接口，默认无 owner。
- 在 pipe 共享 `PipeRingBuffer` 中保存 async owner/signal，使读端设置后写端也能看到同一份状态。
- `F_GETOWN/F_SETOWN` 支持进程和进程组 owner 表示。
- `F_GETOWN_EX/F_SETOWN_EX` 支持 `F_OWNER_TID/F_OWNER_PID/F_OWNER_PGRP` 的 `struct f_owner_ex` 读写。
- `F_GETSIG/F_SETSIG` 保存并返回当前异步 I/O 信号，`0` 表示默认 `SIGIO`。
- pipe 写入数据并唤醒 reader 后，根据 owner 类型投递信号：
  - `F_OWNER_TID`：投递到指定 tid。
  - `F_OWNER_PID`：投递到指定进程/线程组。
  - `F_OWNER_PGRP`：按 `pgid` 遍历进程组投递。
- 实现 `send_signal_to_process_group()`，避免进程组 owner 路径触发 `todo!()`。

## 涉及文件

- `os/src/syscall/fs/fd_ops.rs`
- `os/src/fs/vfs.rs`
- `os/src/fs/files/pipe/mod.rs`
- `os/src/fs/files/pipe/ring_buffer.rs`
- `os/src/fs/files/pipe/file_impl.rs`
- `os/src/signal/mod.rs`

## 验证

已执行：

```text
make
```

结果：默认 LoongArch64 构建通过。

用户随后运行 `make run` 并更新 `log.ans`。最新日志中 musl/glibc 两轮 `fcntl31` 均通过：

```text
fcntl31     1  TPASS  :  fcntl test F_GETOWN, F_SETOWN for process ID success
fcntl31     2  TPASS  :  fcntl test F_GETOWN, F_SETOWN for process group ID success
fcntl31     3  TPASS  :  fcntl test F_GETOWN_EX, F_SETOWN_EX for thread ID success
fcntl31     4  TPASS  :  fcntl test F_GETOWN_EX, F_SETOWN_EX for process ID success
fcntl31     5  TPASS  :  fcntl test F_GETOWN_EX, F_SETOWN_EX for process group ID success
Summary:
passed   5
failed   0
broken   0
skipped  0
warnings 0
```

`FAIL LTP CASE fcntl31 : 10` 是 initproc 包装层打印的退出码行；判断结果以 LTP 内部 `TPASS` 和 `Summary` 为准。
