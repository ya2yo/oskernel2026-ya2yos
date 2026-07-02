# LTP copy_file_range03 时间戳更新

## 背景

LTP `copy_file_range03` 验证 `copy_file_range()` 成功向目标文件写入数据后，目标文件的修改时间会随之更新。该用例包含两个 variant：

- `Testing libc copy_file_range()`：通过 libc wrapper 调用。
- `Testing __NR_copy_file_range syscall`：直接通过 syscall 号调用。

测试会对同一个目标文件先 `fstat(fd_dest)` 读取 `st_mtim`，再 `usleep(1500000)`，随后复制 32 字节到目标文件，最后再次 `fstat(fd_dest)` 并计算两个时间戳的微秒差。差值需要落在 1 秒到 30 秒之间。

## 现象

新的 `log.ans` 中 musl 两个 variant 通过，glibc 的 libc wrapper variant 也通过，但 glibc 的 raw syscall variant 失败：

```text
copy_file_range.h:39: TINFO: Testing __NR_copy_file_range syscall
copy_file_range03.c:52: TFAIL: diff_us = 0, copy_file_range might not update timestamp
```

日志里 raw syscall 分支的 `ClockNanosleep`、`CopyFileRange ret = 32` 和第二次 `fstat/statx` 都成功返回，说明失败不是复制错误，而是用户态看到的目标文件时间戳没有变化。

## 分析

`copy_file_range03.c` 使用的是 `fstat(fd_dest).st_mtim`，不是 `statx` 字段。Ya2yOS 的 ext4 `Kstat` 当前只返回秒级 `st_mtime`，纳秒字段为 0，因此同一秒内的两次修改会被用户态视为完全相同。

`sys_copy_file_range()` 原实现成功写入后只执行：

```rust
outfile
    .inode
    .set_timestamps(None, Some((get_time_ms() / 1000) as u64), None);
```

这里有三个问题：

- 只更新 `mtime`，没有同步更新写数据应触发的 `ctime`。
- 忽略 `set_timestamps()` 返回值，即使元数据更新失败也会继续返回复制字节数。
- 时间戳只取秒级当前值，第二个 variant 复用前一个 variant 已写过的 `file_dest` 时，如果再次复制仍落在同一秒，`fstat` 前后就会得到相同 `st_mtim`。

进一步检查 `sys_clock_nanosleep()` 时发现更直接的触发因素：ticks 到微秒的换算写成了：

```rust
let elapsed_us = (elapsed_ticks * 1_000_000) / (get_clock_freq() / 1000);
```

该表达式实际比微秒值大约 1000 倍，使 `usleep(1500000)` 只等待约 1.5ms 就提前返回。这样 `copy_file_range03` 预期的 1.5 秒间隔没有真正发生，raw syscall 分支更容易落在同一秒内，最终得到 `diff_us = 0`。

## 根因

根因是时间相关路径有两处语义缺口叠加：

1. `clock_nanosleep()` 的 elapsed tick 到微秒换算错误，导致 LTP 的 1.5 秒等待提前返回。
2. `copy_file_range()` 写成功后只设置秒级 `mtime`，没有保证相对旧 `mtime` 前进，也没有更新 `ctime` 和传播元数据更新错误。

glibc raw syscall 分支失败不是 glibc wrapper 行为差异，而是同一 glibc 测例的第二个 variant 直接调用 `__NR_copy_file_range`，并复用前一轮已经存在的目标文件，更容易暴露“同一秒内写入后 fstat 时间戳不变”的问题。

## 修复

涉及文件：

| 文件 | 修改 |
|------|------|
| `os/src/syscall/task/schedule.rs` | 将 `clock_nanosleep()` 中 elapsed ticks 换算为微秒的公式改为 `elapsed_ticks * 1_000_000 / get_clock_freq()` |
| `os/src/syscall/fs/io.rs` | `copy_file_range()` 成功写入后取当前秒与旧 `mtime + 1` 的较大值，同时更新目标 inode 的 `mtime` 和 `ctime`，并用 `?` 传播 `set_timestamps()` 错误 |

修复后的 `copy_file_range()` 时间戳更新逻辑：

```rust
let copied_at = (get_time_ms() / 1000) as u64;
let old_mtime = out_stat.st_mtime as u64;
let timestamp = copied_at.max(old_mtime.saturating_add(1));
outfile
    .inode
    .set_timestamps(None, Some(timestamp), Some(timestamp))?;
```

由于当前 ext4 `fstat()` 对普通文件时间戳仍暴露开机秒语义，`copy_file_range()` 继续使用 `get_time_ms() / 1000`，避免把 inode 时间戳突然改成 epoch 秒后让 LTP 得到超出 30 秒上限的差值。后续若统一文件系统时间为 `CLOCK_REALTIME` epoch 秒，需要同时迁移文件创建、`write()`、`utimensat()`、`fstat/statx` 的时间基准。

## 验证

已执行：

```text
make TARGET_ARCH=loongarch64
timeout 120s make run > log.ans 2>&1
git diff --check
```

结果：

- LoongArch64 构建通过。
- `make run` 正常 `shutdown!`。
- musl `copy_file_range03`：`passed 2 failed 0 broken 0 skipped 0 warnings 0`。
- glibc `copy_file_range03`：`passed 2 failed 0 broken 0 skipped 0 warnings 0`。
- `git diff --check` 无输出。

日志中 `FAIL LTP CASE copy_file_range03 : 10` 和 `RESULT GLIBC LTP SINGLE CASE copy_file_range03 : 10` 是当前 initproc 包装层输出格式，真实结果以 LTP `TPASS/TFAIL` 和 Summary 为准。
