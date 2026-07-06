# fcntl14 POSIX record lock SEEK_CUR 与阻塞语义修复

## 背景

LTP `fcntl14` 覆盖 POSIX advisory record lock 的大量边界场景，包括：

- 全文件、起始边界、结束边界、交叉区间锁冲突。
- `l_whence = SEEK_CUR` 时基于当前文件偏移计算锁区间。
- 负 `l_start` 与负 `l_len` 的合法区间表达。
- `F_SETLKW` 阻塞等待父进程释放锁。
- 非法 `l_whence` 应返回 `EINVAL`。

该用例依赖 Linux `fcntl(F_SETLK/F_SETLKW/F_GETLK)` 对 `struct flock` 的精确区间语义。

## 现象

修复前的 `log.ans` 中，`fcntl14` 前 36 个普通区间用例已有改善，但从第 37 个负偏移相关用例开始仍大量失败，典型现象包括：

```text
fcntl14.c:788: First parent lock failed
fcntl14.c:789: Test case 37, errno = 22
fcntl14.c:673: GETLK: pid = 0, should be parent's id
fcntl14.c:681: GETLK: type = 2, should be parent's first lock type
fcntl14.c:714: SETLK: rc = 0 ... -1/EAGAIN or EACCES was expected
fcntl14.c:1004: Lock succeeded when it should have failed
```

同一组修复前，`F_SETLKW` 仍按非阻塞 `F_SETLK` 行为返回 `EAGAIN`，会导致阻塞锁场景失败；进程关闭文件或退出后 POSIX record lock 也没有被释放，容易污染后续 `fcntl` 用例。

## 分析

`fcntl14.c` 在第 37 组起先执行 `lseek(fd, 5, SEEK_SET)`，再使用 `l_whence = SEEK_CUR` 和负 `l_start/l_len` 构造父进程锁。例如：

- `l_whence = SEEK_CUR, l_start = -3, l_len = 2` 表示锁 `[2, 3]`。
- `l_whence = SEEK_CUR, l_start = 2, l_len = -2` 表示锁 `[6, 7]`。
- `l_whence = SEEK_CUR, l_start = 2, l_len = -5` 表示锁 `[3, 7]`。

原 `file_lock::to_absolute()` 只正确处理了 `SEEK_SET` 和部分 `SEEK_END`：

- 遇到 `SEEK_CUR` 时退化为按 `SEEK_SET` 处理，没有读取当前 fd offset。
- 负 `l_len` 被直接视为非法区间返回 `EINVAL`。
- 非法 `l_whence` 没有直接返回 `EINVAL`，导致 negative whence 用例错误成功。
- `F_SETLKW` 没有真正阻塞等待，也没有唤醒或死锁检测。
- record lock 没有在 `close/close_range/exit` 时按进程释放。

因此父进程锁区间计算错误，子进程 `F_GETLK/F_SETLK` 看到的冲突状态与 Linux 不一致。

## 根因

POSIX record lock 实现缺少 Linux 兼容的区间换算和生命周期语义：

- `SEEK_CUR` 需要 syscall 层传入当前打开文件描述的 offset。
- `l_len < 0` 是合法表达，应换算为反向闭区间 `[start + len + 1, start]`。
- `l_whence` 只能是 `SEEK_SET/SEEK_CUR/SEEK_END`，其它值必须返回 `EINVAL`。
- `F_SETLKW` 应在冲突时阻塞等待锁释放，出现等待环时返回 `EDEADLK`。
- POSIX record lock 按进程持有，关闭同 inode 的任意 fd 或进程退出时需要释放该进程在该文件上的锁。

## 修复

- `file_lock::to_absolute()`：
  - 增加 `current_offset` 参数。
  - 支持 `SEEK_CUR` 基于当前 fd offset 计算起点。
  - 支持负 `l_len`，按 Linux 语义换算反向区间。
  - 非法 `l_whence` 和负绝对起点返回 `EINVAL`。
- `sys_fcntl()` record lock 分支：
  - 在 `F_GETLK/F_SETLK/F_SETLKW/OFD_*` 中通过 `lseek(0, SEEK_CUR)` 读取当前文件偏移。
  - 将当前偏移传入 `file_lock::getlk/setlk`。
  - 保持先 `copy_from_user()` 再解析普通文件对象的错误优先级。
- `F_SETLKW`：
  - 冲突时登记等待边，注册 waker 并阻塞等待。
  - 检测等待图中是否形成环，形成环时返回 `EDEADLK`。
  - 锁转换、释放或进程退出后唤醒等待者。
- lock 生命周期：
  - `close()` / `close_range()` 关闭普通文件 fd 时释放当前进程在该 inode 上的 POSIX record locks。
  - 进程最终退出时兜底释放该 pid 的所有 POSIX record locks。

## 涉及文件

- `os/src/syscall/fs/file_lock.rs`
- `os/src/syscall/fs/fd_ops.rs`
- `os/src/syscall/fs/mod.rs`
- `os/src/syscall/mod.rs`
- `os/src/task/mod.rs`

## 验证

已执行构建验证：

```text
make TARGET_ARCH=riscv64
make
```

结果：

- RISC-V 构建通过。
- 默认 LoongArch64 构建通过。
- 维护者确认后续运行中 `fcntl14` 已通过。

说明：当前仓库中的最新 `log.ans` 已切换到后续 `fcntl23` 起的测试输出，不再包含 `fcntl14` 段，因此本条记录不引用该文件作为 `fcntl14` 的直接日志来源。
