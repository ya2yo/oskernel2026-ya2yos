# fcntl01 F_GETFL/F_SETFL 文件状态标志修复

## 背景

LTP `fcntl01` 验证 `fcntl(2)` 的基础 fd 操作：

- `F_DUPFD` 分配不小于指定下界的新 fd；
- `F_GETFL` 能返回打开文件时的访问模式；
- `F_SETFL` 能设置 `O_NDELAY/O_NONBLOCK` 和 `O_APPEND` 等文件状态标志；
- `F_SETFD/F_GETFD` 能设置和读取 `FD_CLOEXEC`。

## 现象

新的 `log.ans` 中，musl/glibc 两轮 `fcntl01` 均失败在 `F_GETFL/F_SETFL`：

```text
fcntl01.c:99: unexpected flag 0x2, expected 0x1
fcntl01.c:117: unexpected flag ox2, expected 0x401
fcntl01.c:136: unexpected flag 0x2, expected 0x1
```

测试用例以 `O_WRONLY | O_CREAT` 打开文件，期望 `F_GETFL` 至少包含 `O_WRONLY`；设置 `O_APPEND` 后，期望再次读取时包含 `O_APPEND | O_WRONLY`。

## 分析

当前 `sys_fcntl(F_GETFL)` 直接硬编码返回 `OpenFlags::O_RDWR`，只额外根据 `file.non_block()` 拼上 `O_NONBLOCK`：

```text
let mut res = OpenFlags::O_RDWR.bits()
```

因此无论文件最初是 `O_WRONLY` 还是 `O_RDONLY`，用户态都只能看到 `0x2`。同时 `F_SETFL` 只同步了 fd 表和底层 `File` 的 nonblock 状态，没有保存 `O_APPEND`，所以 `F_SETFL(O_APPEND)` 后 `F_GETFL` 仍然看不到 append 位。

## 根因

`FileDescriptor` 已经保存了打开时的 `OpenFlags`，但 `F_GETFL` 没有读取它；`F_SETFL` 也没有更新 fd 描述符中的文件状态标志。结果访问模式丢失，`O_APPEND/O_NONBLOCK` 等状态位无法按 Linux 语义持久可见。

## 修复

涉及文件：

- `os/src/fs/fstruct.rs`
- `os/src/syscall/fs/fd_ops.rs`

主要改动：

- `FileDescriptor::getfl_flags()` 返回 `F_GETFL` 可见的访问模式和文件状态标志，过滤掉 `O_CREAT/O_EXCL/O_TRUNC/O_CLOEXEC` 等创建或 fd descriptor 标志；
- `FileDescriptor::set_status_flags()` 与 `FdTable::set_status_flags()` 只更新 `F_SETFL` 可修改的状态位，包括 `O_APPEND/O_NONBLOCK/O_ASYNC/O_DIRECT/O_NOATIME`；
- `sys_fcntl(F_GETFL)` 改为返回 fd 描述符保存的真实 flags；
- `sys_fcntl(F_SETFL)` 先更新 fd 描述符状态位，再同步底层文件对象的 nonblocking 状态。

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml
make
```

当前默认架构为 LoongArch64，`make` 通过。

用户随后提供新的 `log.ans`。日志中 musl `fcntl01`：

```text
FAIL LTP CASE fcntl01 : 0
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

glibc `fcntl01`：

```text
RESULT GLIBC LTP SINGLE CASE fcntl01 : 0
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

本次未重新运行 QEMU；以用户更新后的 `log.ans` 为验证依据。
