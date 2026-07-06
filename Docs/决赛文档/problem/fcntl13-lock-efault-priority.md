# fcntl13 record lock EFAULT 优先级修复

## 背景

LTP `fcntl13` 验证 `fcntl(2)` 的基础错误处理，包括坏 `struct flock *` 指针返回 `EFAULT`、未知命令返回 `EINVAL`、非法 `l_whence` 返回 `EINVAL`、坏 fd 返回 `EBADF`。

## 现象

新的 `log.ans` 中，musl 和 glibc 两轮 `fcntl13` 只有第一项失败：

```text
fcntl13.c:47: TFAIL: fcntl(1, F_SETLK, flock) expected EFAULT: EINVAL (22)
```

其余 `F_BADCMD`、非法 `l_whence` 和坏 fd 用例均通过。

## 分析

`fcntl13` 第一项使用 `fd=1`、`cmd=F_SETLK` 和 `tst_get_bad_addr()` 返回的坏 `struct flock *`。Linux 语义要求 syscall 读取 `flock` 参数时先发现用户指针不可访问，返回 `EFAULT`。

原 `sys_fcntl()` 的 record lock 分支先执行：

1. `fd_table.get(fd)`。
2. `file.file()`，要求 fd 指向普通 `OSFile`。
3. 再 `copy_from_user()` 读取 `struct flock`。

`fd=1` 是 stdout，不是普通文件；因此 `file.file()` 先返回 `EINVAL`，遮蔽了坏用户指针的 `EFAULT`。

## 根因

record lock 分支的错误优先级不符合 LTP/Linux 预期：对需要用户 `struct flock *` 的命令，应先复制并校验用户结构，再进入普通文件对象解析和锁语义处理。

## 修复

- `F_GETLK/F_GETLK64`、`F_SETLK/F_SETLK64`、`F_SETLKW/F_SETLKW64` 分支先 `copy_from_user()` 读取 `struct flock`，再获取普通文件 inode。
- OFD lock 分支也按相同顺序处理，保持错误优先级一致。
- 保持未知命令仍在 match 前返回 `EINVAL`，坏 fd 仍由 `fd_table.get(fd)?` 先返回 `EBADF`。

## 涉及文件

- `os/src/syscall/fs/fd_ops.rs`

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

结果：

- 默认 LoongArch64 `make` 通过。
- 最新 `log.ans` 中 musl `fcntl13`：

```text
Summary:
passed   4
failed   0
broken   0
skipped  0
warnings 0
```

- 最新 `log.ans` 中 glibc `fcntl13`：

```text
Summary:
passed   4
failed   0
broken   0
skipped  0
warnings 0
```
