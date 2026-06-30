# write04: FIFO 非阻塞写满后未返回 EAGAIN

## 背景

LTP `write04` 验证 FIFO 的非阻塞写语义：

- 通过 `mknod(S_IFIFO)` 创建命名 FIFO。
- 读端和写端均以 `O_NONBLOCK` 打开。
- 先向 FIFO 写入超过一页的数据填充缓冲区。
- 再次写入 8 页数据时，若 FIFO 已满，应返回 `EAGAIN`。

## 现象

原始 `log.ans` 中 musl/glibc 单跑 `write04` 均失败：

```text
write04.c:31: TFAIL: write(wfd, wbuf, sizeof(wbuf)) succeeded
Summary:
passed   0
failed   1
broken   0
skipped  0
warnings 0
```

glibc 运行段还伴随既有噪声：

```text
write_back_cache ext4_fopen: /tmp/LTP_wriePfDLB/.3, rc = 2
Fail to convert LoongArch Unknown to Trap type! 0x0
```

但真正的测试失败点是 `write04.c:31` 的 `TFAIL`。

## 分析

读取 LTP `write04.c` 后确认，测试不是普通文件短写场景，而是命名 FIFO 的 `O_NONBLOCK` 场景。Linux 语义下，FIFO/pipe 写端在非阻塞模式且没有足够空间完成本次写入时，应失败并返回 `EAGAIN`，而不能报告成功。

内核侧检查发现两个问题：

1. `mknodat(S_IFIFO)` 虽然已经在 `FsIndex` 中登记特殊节点类型，但 `openat()` 后仍通过普通 `open()` 返回 `OSFile`，后续 `write()` 实际落到 ext4 普通文件写路径。
2. 现有 `Pipe` 只按阻塞管道实现，端点关闭判断依赖创建时保存的单个 `Weak<Pipe>`，无法正确表达命名 FIFO 多次 `open()` 后的读端/写端数量，也没有实现 `O_NONBLOCK` 读写返回 `EAGAIN` 的语义。

因此 `write04` 预填 FIFO 时实际上在写普通文件；第二次 `write()` 也继续成功，导致 LTP 报告 `write()` succeeded。

## 根因

Ya2yOS 对命名 FIFO 的类型只用于 `lseek/fstat` 等元数据语义，未将 FIFO 打开后的读写对象切换为管道对象；同时 `Pipe` 没有将 fd 的 `O_NONBLOCK` 状态参与读写阻塞决策。

## 修复

涉及文件：

- `os/src/fs/files/pipe.rs`
- `os/src/fs/mod.rs`
- `os/src/syscall/fs/fd_ops.rs`

主要修改：

- 在 pipe 模块中新增按路径管理的 FIFO 缓冲区表，`open_fifo(path, flags)` 为同一路径的 FIFO open 返回共享 `PipeRingBuffer` 上的读端、写端或读写端。
- `sys_openat()` 在普通 `open()` 完成路径解析和权限检查后，若发现目标为 FIFO，则返回 `FileClass::Abs(open_fifo(...))`，避免 FIFO 继续走普通 `OSFile::write()`。
- `Pipe` 增加 `nonblocking` 状态，并实现 `File::nonblocking()` / `set_nonblocking()`，使 `open(O_NONBLOCK)` 与后续 `fcntl(F_SETFL)` 都能影响 pipe 读写行为。
- `Pipe::read()` 在空管道且仍有写端时，非阻塞模式返回 `EAGAIN`。
- `Pipe::write()` 在缓冲区已满且仍有读端时，非阻塞模式返回 `EAGAIN`；读端全部关闭时仍返回 `EPIPE`。
- 将原先基于单个弱引用的端点关闭判断改为读端/写端计数，支持命名 FIFO 多次打开和匿名 pipe 的端点生命周期。
- `open_fifo()` 对 `O_WRONLY | O_NONBLOCK` 且当前没有读端的情况返回 `ENXIO`，补齐 FIFO 非阻塞打开的基础语义。

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml --all
make
```

结果：

- `make` 通过，当前默认 `TARGET_ARCH=loongarch64`。
- 运行 `make run` 时，当前沙箱内 QEMU 因 `/var/tmp` 只读无法创建临时文件，未能在 AI 环境中完成运行验证：

```text
qemu-system-loongarch64: ... Could not open temporary file '/var/tmp/...': Read-only file system
```

- 维护者随后在本地完成复现配置运行，并确认 `write04` 结果已通过。
- 未运行 `riscv64` 验证。
