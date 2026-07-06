# fcntl35 pipe-max-size 限制初始 pipe 容量

## 背景

LTP `fcntl35` 覆盖 Linux commit `086e774a57fb` 的回归场景：非特权用户虽然不能通过 `F_SETPIPE_SZ` 把 pipe 容量调到 `/proc/sys/fs/pipe-max-size` 以上，但历史上仍可能通过新建 pipe 得到超过该 sysctl 上限的默认容量。测试会把 `/proc/sys/fs/pipe-max-size` 临时写成 `getpagesize()`，然后分别以 `nobody` 和 root 身份创建 pipe 并读取 `F_GETPIPE_SZ`。

## 现象

`log.ans` 中 musl 和 glibc 轮次均失败：

```text
fcntl35.c:83: TFAIL: an unprivileged user init the capacity of a pipe to 65536 unexpectedly, expected 4096
fcntl35.c:87: TPASS: a privileged user init the capacity of a pipe to 65536 successfully
Summary:
passed   1
failed   1
broken   0
```

说明特权用户默认容量符合预期，但非特权用户创建 pipe 时没有受 `pipe-max-size=4096` 约束。

## 分析

此前 pipe size 兼容性实现已经为 `PipeRingBuffer` 增加 per-pipe `capacity`，并支持 `F_GETPIPE_SZ/F_SETPIPE_SZ`，但仍存在两个缺口：

- `/proc/sys/fs/pipe-max-size` 是启动期创建的普通文件，用户写入后只改变文件内容，没有同步到 pipe 子系统。
- `make_pipe()` 仍固定使用 `PIPE_DEFAULT_SIZE=65536` 初始化 ring buffer，没有区分当前任务是否具备 `CAP_SYS_RESOURCE`。

`fcntl35` 的 `setup()` 会通过 `SAFE_FILE_PRINTF("/proc/sys/fs/pipe-max-size", "%d", getpagesize())` 修改 sysctl；之后子进程 `setuid(nobody)`，能力集被清空，新建 pipe 的初始容量应取 `min(PIPE_DEFAULT_SIZE, pipe_max_size)`。root 或仍具备 `CAP_SYS_RESOURCE` 的任务不受该初始容量上限影响，仍可得到默认 `65536`。

第一次修复后 glibc 轮次通过，但 musl 在 `fclose()` 上返回 `EINVAL`。原因是 stdio/writev 路径可能向 `File::write` 传入空 slice，sysctl 解析把空 slice 误判为非法输入。空写不应改变 sysctl 状态，也不应失败。

## 根因

`pipe-max-size` 的 VFS 文件内容和内核 pipe 容量策略没有建立状态连接；匿名 pipe 创建路径只使用固定默认容量，没有按当前任务能力和 sysctl 上限计算非特权初始容量。同时，sysctl 写入 hook 对空写片段处理过严，导致 musl stdio flush 路径出现 `TBROK`。

## 修复

- `os/src/fs/files/pipe/mod.rs`
  - 增加 `PIPE_MAX_SIZE_SYSCTL: AtomicUsize` 保存当前 `/proc/sys/fs/pipe-max-size`。
  - 增加 `set_pipe_max_size()` 和 `pipe_max_size()`。
  - 增加 `CAP_SYS_RESOURCE` 检查，使用 task effective capability 判断特权，而不是只看 `euid==0`。
  - `make_pipe()` 对有 `CAP_SYS_RESOURCE` 的任务保留默认 `65536`；非特权任务使用 `min(PIPE_DEFAULT_SIZE, pipe_max_size())`。
  - `F_SETPIPE_SZ` 继续检查固定实现上限，同时非特权任务请求超过当前 `pipe-max-size` 时返回 `EPERM`。
- `os/src/fs/files/pipe/ring_buffer.rs`
  - 增加 `PipeRingBuffer::with_capacity()`，让创建 pipe 时传入初始容量。
- `os/src/fs/files/os_file.rs`
  - 在普通文件写入路径中识别 `/proc/sys/fs/pipe-max-size`，解析写入的数字并同步到 pipe 子系统。
  - 忽略空 slice，避免 musl stdio/writev flush 空片段导致 `EINVAL`。

## 涉及文件

- `os/src/fs/files/pipe/mod.rs`
- `os/src/fs/files/pipe/ring_buffer.rs`
- `os/src/fs/files/os_file.rs`

## 验证

已执行：

```text
make
timeout 120s make run
```

结果：

- 默认 LoongArch64 `make` 通过；构建中仍有既有 `smoltcp` vendor warning，与本次修改无关。
- `make run` 单跑 `fcntl35`：

```text
fcntl35.c:87: TPASS: an unprivileged user init the capacity of a pipe to 4096 successfully
fcntl35.c:87: TPASS: a privileged user init the capacity of a pipe to 65536 successfully
Summary:
passed   2
failed   0
broken   0
skipped  0
warnings 0
```

musl 与 glibc 两轮均为 `passed 2 failed 0 broken 0`。日志中仍出现 `FAIL LTP CASE fcntl35 : 10` / `RESULT GLIBC LTP SINGLE CASE fcntl35 : 10` wrapper 行，但按项目规则以 `TPASS` 和 `Summary` 为准。
