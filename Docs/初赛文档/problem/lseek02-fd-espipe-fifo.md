# lseek02: fd 错误码与 FIFO ESPIPE 语义

## 背景

LTP `lseek02` 验证 `lseek(2)` 的错误码优先级和不可 seek 文件描述符语义：

- 无效 fd 应返回 `EBADF`。
- 非法 `whence` 应返回 `EINVAL`。
- pipe/FIFO fd 应返回 `ESPIPE`。

本次在 LoongArch 单跑 musl/glibc `lseek02` 时，多个断言失败。

## 现象

原始 `log.ans` 中 musl/glibc 均出现：

```text
lseek02.c:67: TFAIL: lseek(-1, 1, 0) failed unexpectedly, expected EBADF: EINVAL (22)
lseek02.c:58: TFAIL: lseek(4, 1, 0) succeeded unexpectedly
lseek02.c:67: TFAIL: lseek(5, 1, 0) failed unexpectedly, expected ESPIPE: EINVAL (22)
lseek02.c:58: TFAIL: lseek(7, 1, 0) succeeded unexpectedly
```

其中 fd 4/7 来自 `mkfifo()` / `mknod(S_IFIFO)` 后 `open()` 的命名 FIFO，fd 5 来自匿名 pipe。

## 分析

检查 LTP 源码确认，测试先构造普通文件、命名 FIFO、匿名 pipe，再对这些 fd 调用 `lseek(fd, 1, SEEK_SET/SEEK_CUR/SEEK_END)`。

内核侧存在三处问题：

1. `sys_lseek()` 手写 fd 检查，fd 无效时返回 `EINVAL`，没有使用 fd 表统一的 `EBADF` 语义。
2. `sys_lseek()` 通过 `FileDescriptor::file()` 只允许 `OSFile`，抽象 fd/pipe/socket 等不可 seek 对象没有统一 `lseek` 错误路径。
3. `mknodat()` 创建 FIFO 时底层 ext4 实际仍以普通文件承载；`ext4_mode_set()` 不能把该文件转换成真正的 FIFO，导致后续 `open()` 后 `OSFile::lseek()` 仍按普通文件更新 offset。

## 根因

Ya2yOS 的 `lseek` 路径缺少 Linux 兼容的错误码分层：

- fd 查找失败应该先返回 `EBADF`。
- fd 有效但对象不支持 seek 时应返回 `ESPIPE`。
- 只有可 seek 文件的非法 `whence` / 负 offset 才返回 `EINVAL`。

同时，当前 ext4 特殊节点支持不完整，命名 FIFO 类型信息没有在 VFS 层保留下来，导致 `lseek` 无法识别通过 `mkfifo/mknod` 创建的 FIFO。

## 修复

涉及文件：

- `os/src/syscall/fs/io.rs`
- `os/src/fs/vfs.rs`
- `os/src/fs/files/os_file.rs`
- `os/src/syscall/fs/ctl.rs`
- `os/src/fs/kernel_fs_ops/fsidx.rs`

主要修改：

- `sys_lseek()` 改为先通过 `fd_table.get(fd)?` 获取 fd，保证无效 fd 返回 `EBADF`；随后使用 `FileClass::any()` 调用 trait `lseek`。
- `File::lseek()` 默认返回 `ESPIPE`，避免 pipe/socket/抽象文件落到 `unimplemented!` 或错误码不一致。
- `OSFile::lseek()` 对 FIFO/socket 类型返回 `ESPIPE`，并修复 `SEEK_SET` 负 offset 被 cast 成超大 `usize` 的问题。
- `mknodat()` 对 FIFO/设备/socket 创建保留 `S_IF*` 类型位，并在 `FsIndex` 中登记特殊节点路径类型。
- `FsIndex::remove_inode_idx()` 同步清理特殊节点类型登记，避免 unlink 后路径类型残留。

## 验证

已执行：

```text
rustfmt os/src/fs/kernel_fs_ops/fsidx.rs os/src/syscall/fs/ctl.rs os/src/fs/files/os_file.rs os/src/syscall/fs/io.rs os/src/fs/vfs.rs
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 通过，当前默认 `TARGET_ARCH=loongarch64`。
- LoongArch 当前配置单跑 musl/glibc `lseek02`，两者均通过：

```text
Summary:
passed   15
failed   0
broken   0
skipped  0
warnings 0
```

- 新 `log.ans` 中所有 `lseek02.c` 断言均为 `TPASS`，未再出现 `TFAIL`。
- 未运行 `riscv64` 验证。
