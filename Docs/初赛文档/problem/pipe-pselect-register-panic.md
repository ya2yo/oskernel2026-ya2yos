# pipe pselect6 register panic

## 背景

`pselect6()` 已改为 `block_on(poll_fn(...))` 的阻塞等待模型。fd 首次 poll 未就绪时，内核会调用对应文件对象的 `File::register(cx, interests)` 注册 waker，等待后续 I/O 事件唤醒。

## 现象

GDB 回溯显示内核在 `sys_pselect6()` 中注册 pipe fd watch 时 panic：

```text
os::fs::vfs::File::register<os::fs::files::pipe::Pipe>
os::syscall::io_mpx::select::register_watch_entries
os::syscall::io_mpx::select::sys_pselect6
```

panic 点位于 `src/fs/vfs.rs` 的默认 `File::register()`，该默认实现是 `unimplemented!("File::register")`。

## 分析

`select/pselect6` 的阻塞化依赖所有可等待 fd 实现两部分语义：

- `poll(events)`：立即返回当前 ready 状态。
- `register(cx, events)`：未 ready 时保存当前 waker，后续状态变化时唤醒。

网络 socket、eventfd、inotify 等对象已经实现了 `register()`，但 `Pipe` 只维护了同步阻塞 `read()/write()` 使用的 `read_waiters/write_waiters` 任务队列，没有实现 future-waker 路径。因此 `pselect6` 监听 pipe 时会落到 trait 默认实现并 panic。

同时，pipe 的 `poll()` 已经会在端点关闭时返回 `HUP/ERR`，但端点关闭前如果已有 `pselect6` 正在等待，也需要主动唤醒对应 waker，否则修掉 panic 后仍可能睡死。

## 根因

`Pipe` 没有实现 `File::register()`，并且缺少与 `pselect6` 阻塞 future 配套的 `PollSet` 唤醒机制。

## 修复

涉及文件：

| 文件 | 修改 |
|------|------|
| `os/src/fs/files/pipe.rs` | 在 `PipeRingBuffer` 中增加 `read_poll/write_poll` 两个 `PollSet` |
| `os/src/fs/files/pipe.rs` | `read()` 释放写空间时唤醒写端 waker，`write()` 写入数据后唤醒读端 waker |
| `os/src/fs/files/pipe.rs` | 实现 `File::register()`，按 `IN/HUP` 注册读端 waker，按 `OUT/ERR` 注册写端 waker |
| `os/src/fs/files/pipe.rs` | 增加 `Drop for Pipe`，读端或写端最后关闭时唤醒对端所有阻塞任务和 poll waker |
| `os/src/fs/files/pipe.rs` | `write()` 在所有读端关闭时返回 `EPIPE`，避免满管道写者继续阻塞 |

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 通过。
- `make run` 在 120 秒窗口内跑到 lmbench 输出 `Pipe latency`，随后被外部 timeout 终止。
- `log.ans` 中未再出现 `panic`、`File::register`、`unimplemented`、`src/fs/vfs.rs:163` 或 `select.rs:147`。
- 日志中仍有用户态 `StorePageFault` warn 循环，这是已有的用户态 fault 行为，不是本次 `Pipe::register` panic。
