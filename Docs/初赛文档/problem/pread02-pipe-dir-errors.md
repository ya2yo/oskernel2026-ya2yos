# pread02: pipe 与目录错误码

## 背景

LTP `pread02` 验证 `pread(2)` 的基础错误处理：

- fd 指向 pipe/FIFO 时返回 `ESPIPE`。
- offset 为负数时返回 `EINVAL`。
- fd 指向目录时返回 `EISDIR`。

## 现象

新的 `log.ans` 中，LoongArch musl/glibc 单跑 `pread02` 均失败：

```text
pread02.c:44: TFAIL: pread(3, 1024, 0) file descriptor is a PIPE or FIFO expected ESPIPE: EINVAL (22)
pread02.c:44: TPASS: pread(5, 1024, -1) specified offset is negative : EINVAL (22)
pread02.c:44: TFAIL: pread(6, 1024, 0) file descriptor is a directory succeeded
```

## 分析

检查 LTP `pread02.c` 可知：

- `fd=3` 为 `pipe_fd[0]`，期望 `pread()` 返回 `ESPIPE`。
- `fd=5` 为普通文件，但 offset 为 `-1`，期望 `EINVAL`。
- `fd=6` 为目录 fd，期望 `EISDIR`。

内核原 `sys_pread64()` 存在两类问题：

1. 通过 `FileDescriptor::file()` 获取 `OSFile`，pipe 属于抽象文件，不是 `OSFile`，该转换返回 `EINVAL`，导致 pipe 场景错误。
2. 对目录 fd 仍走普通 `OSFile` 的 `lseek + read` 路径；目录 inode size 为 0，最终读路径返回成功，未按 Linux 语义返回 `EISDIR`。

同时检查到 `sys_pwrite64()` 与 `sys_pread64()` 结构相同，也存在 pipe 通过 `file()` 转换得到 `EINVAL`、无效 fd 返回 `EINVAL`、只读 fd 返回 `EACCES` 等与 Linux/LTP 不一致的风险，因此一并按相同错误码顺序做了同步修正。

## 根因

`pread64/pwrite64` 仍把“普通文件对象”作为入口，而不是先按 fd 表得到任意 `File`，再根据对象是否支持 seek 判断 `ESPIPE`。此外，`pread64` 缺少目录 fd 的显式 `EISDIR` 检查。

## 修复

涉及文件：

- `os/src/syscall/fs/io.rs`

主要修改：

- `sys_pread64()` 改用 `fd_table.get(fd)?.any()` 获取任意 fd 对象，无效 fd 返回 `EBADF`。
- `sys_pread64()` 保留负 offset 的 `EINVAL`，不可读 fd 返回 `EBADF`。
- `sys_pread64()` 先调用 `lseek(0, SEEK_CUR)` 判断 fd 是否可 seek；pipe/FIFO 等不可 seek fd 由 `File::lseek()` 返回 `ESPIPE`。
- `sys_pread64()` 对目录 fd 检查 `st_mode`，在 read 前返回 `EISDIR`。
- `sys_pread64()` 在临时修改文件 offset 后恢复原 offset，再写回用户缓冲区。
- `sys_pwrite64()` 同步使用 `fd_table.get(fd)?.any()`、不可写返回 `EBADF`、不可 seek 返回 `ESPIPE`，并在 copy/write 失败时尽量恢复原 offset。

## 验证

已执行：

```text
rustfmt os/src/syscall/fs/io.rs
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 通过，当前默认 `TARGET_ARCH=loongarch64`。
- LoongArch 当前配置单跑 musl/glibc `pread02`，两者均通过：

```text
Summary:
passed   3
failed   0
broken   0
skipped  0
warnings 0
```

- 新 `log.ans` 中所有 `pread02.c` 断言均为 `TPASS`，未再出现 `TFAIL`。
- `pwrite64` 只随 `make` 做了编译验证；临时切换 `initproc.rs` 后未重建用户镜像，未得到有效 `pwrite02` 行为验证。
- 未运行 `riscv64` 验证。
