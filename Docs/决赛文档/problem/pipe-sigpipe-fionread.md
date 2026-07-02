# pipe SIGPIPE 与 FIONREAD 语义修复

## 背景

LTP pipe 系列测例覆盖 pipe 基础读写、无读端写入、非阻塞满 pipe、`ioctl(FIONREAD)` 等 Linux 兼容语义。项目在将 pipe 缓冲区改为 `PipeBuf` 片段队列后，基础读写和非阻塞满 pipe 可以运行，但 `log.ans` 显示部分 pipe 测例仍失败。

## 现象

`log.ans` 中与 pipe 核心实现直接相关的失败包括：

- `pipe02`：child 向无读端 pipe 写入后只得到 `EPIPE`，没有被默认 `SIGPIPE` 动作终止，输出 `TFAIL: Child wasn't killed by signal`。
- `pipe08`：测例安装 `SIGPIPE` handler 后向无读端 pipe 写入，`write()` 返回 `EPIPE`，但 handler 没有被调用，输出 `TFAIL: sigpipe_cnt (0) != 1 (1)`。
- `pipe12`：非阻塞满 pipe 写入已经正确返回 `EAGAIN`，但随后 `ioctl(fd, FIONREAD, ...)` 返回 `ENOTTY`，输出 `TBROK: ioctl(4,(0x541B),...) failed: ENOTTY (25)`。

同时日志中还有两个 pipe 测例相关失败，但不属于 pipe 读写核心语义：

- `pipe07` 失败于 `opendir(/proc/self/fd) failed: ENOENT`，需要补 procfs fd 目录。
- `pipe15` 失败于打开 `/proc/sys/fs/pipe-user-pages-soft` 返回 `ENOENT`，需要补 procfs pipe sysctl 文件。

## 分析

Linux 语义要求：向没有读端的 pipe 写入时，内核应向当前写线程投递 `SIGPIPE`，同时 `write()` 返回 `EPIPE`。如果 `SIGPIPE` 使用默认动作，进程会被信号终止；如果安装了 handler，handler 应被调用。

原 `Pipe::write()` 在检测到 `all_read_ends_closed()` 时直接返回 `Err(SysErrNo::EPIPE)`，没有投递 `SIGPIPE`，因此 `pipe02` 和 `pipe08` 失败。

`pipe12` 使用 `FIONREAD` 查询 pipe 中当前可读字节数。原 `Pipe::ioctl()` 只兼容 watch queue 相关命令，对其他命令统一返回 `ENOTTY`，没有处理 `FIONREAD(0x541B)`。

## 根因

`Pipe` 文件对象缺少两个 Linux 可见语义：

- broken pipe 写入路径没有产生 `SIGPIPE`。
- pipe fd 的 `ioctl(FIONREAD)` 未实现。

这两个问题与 `PipeBuf` 片段队列本身无关，属于 pipe 文件对象的 syscall 兼容语义缺口。

## 修复

修改 `os/src/fs/files/pipe.rs`：

- 新增 `Pipe::broken_pipe()`，向当前线程投递 `SigSet::SIGPIPE`，再返回 `SysErrNo::EPIPE`。
- `Pipe::write()` 在初始检查和阻塞前复查读端关闭时，释放 pipe/task 锁后调用 `broken_pipe()`，避免在持锁状态下投递信号。
- `write_kernel_bytes()` 对读端关闭也复用 `broken_pipe()`。
- 新增 `FIONREAD = 0x541B`，在 `Pipe::ioctl()` 中将 `available_read()` 作为 `i32` 写回用户指针并返回成功。

## 验证

已执行：

```text
rustfmt --edition 2024 --unstable-features --skip-children os/src/fs/files/pipe.rs
make
timeout 120s make run > /tmp/splice-pipebuf-run.log 2>&1
```

结果：

- 默认 LoongArch64 `make` 构建通过。
- 构建仍有 vendor `smoltcp` warning，与本次修改无关。
- 当前 `initproc` 运行 pipe 相关 musl 单测：
  - `pipe02.c:77: TPASS: Child killed by SIGPIPE`
  - `pipe08.c:37: TPASS: sigpipe_cnt == 1 (1)`
  - `pipe12` 中非阻塞满 pipe、非空 pipe、空 pipe 场景共 6 个断言均 `TPASS`，不再出现 `FIONREAD` 的 `ENOTTY`。
- `pipe07` 仍因 `/proc/self/fd` 缺失 `TBROK`，`pipe15` 仍因 `/proc/sys/fs/pipe-user-pages-soft` 缺失 `TBROK`，需后续按 procfs 能力单独修复。
