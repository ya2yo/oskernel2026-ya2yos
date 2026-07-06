# fcntl11 POSIX record lock 区间语义修复

## 背景

LTP `fcntl11` 用父进程在同一个文件上设置多个 POSIX record lock，再由子进程通过 `F_GETLK` 查询冲突锁，验证锁类型、起始偏移、长度和持有者 pid。

## 现象

新的 `log.ans` 中，musl 和 glibc 两轮 `fcntl11` 均失败。典型错误包括：

```text
lock type is wrong should be F_RDLCK is F_WRLCK
region starts in wrong place, should be 1 is 10
region length is wrong, should be 0 is 5
locking pid is wrong
fcntl on file failed: errno=EAGAIN/EWOULDBLOCK(11)
```

失败说明内核返回了错误的冲突锁，且同一进程在已有锁上设置重叠锁时被错误判定为冲突。

## 分析

`fcntl11` 的关键模式是：

- 父进程先设置写锁，例如 `[10, 14]`。
- 父进程再设置读锁，可能与已有写锁相邻、部分重叠或完全包含其中一段。
- 子进程用 `F_GETLK` 查询写锁请求会被哪个父进程锁阻止。

Linux/POSIX record lock 是按进程持有的。同一进程后续设置的锁不会和自己冲突，而是会转换已有锁：重叠区间被新锁覆盖，旧锁只保留未覆盖的左右两段。`F_GETLK` 查询时也应忽略调用进程自己的锁，并返回查询范围内最靠前的冲突锁。

原实现存在三处问题：

- `F_SETLK` 把用户传入 `struct flock.l_pid` 直接保存为锁持有者，而用户设置锁时该字段通常为 0；内核应保存当前进程 pid。
- 同一进程再次设置重叠锁时，原实现仍按冲突处理，导致 `EAGAIN`。
- `F_GETLK` 按插入顺序返回冲突锁，父进程先插入 `[10, 14]` 写锁再插入前面的读锁时，子进程查询会错误返回起始偏移 10 的写锁。

## 根因

`file_lock.rs` 只实现了“全局锁表 + 简单冲突检测”，没有实现 POSIX record lock 的同进程锁转换、区间拆分/合并和按起始偏移选择冲突锁，也没有由 syscall 层传入真实 owner pid。

## 修复

- `sys_fcntl()` 在 record lock 分支中取 `current_task().pid()`，传给 `file_lock::setlk/getlk`。
- `file_lock::setlk()`：
  - 校验 `F_RDLCK/F_WRLCK/F_UNLCK` 类型。
  - 只把不同 owner pid 的冲突锁作为 `EAGAIN` 条件。
  - 对同 owner pid 的重叠锁做覆盖转换，保留未覆盖的左段和右段。
  - 新锁插入后按 `(start, end, pid, type)` 排序，并合并同 owner、同类型且相邻或重叠的区间。
- `file_lock::getlk()`：
  - 忽略同 owner pid 的锁。
  - 在所有冲突锁中返回起始偏移最小的锁。
- `Flock::to_bytes()` 将 `l_pid` 写入标准 offset 和尾部 padding offset，兼容不同 libc/架构组合的 `struct flock` 布局差异。

## 涉及文件

- `os/src/syscall/fs/fd_ops.rs`
- `os/src/syscall/fs/file_lock.rs`

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

结果：

- 默认 LoongArch64 `make` 通过。
- 最新 `log.ans` 中 musl `fcntl11`：

```text
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

- 最新 `log.ans` 中 glibc `fcntl11`：

```text
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```
