# fcntl33 文件租约 break 通知修复

## 背景

LTP `fcntl33` 验证 `fcntl(F_SETLEASE)` 的 lease break 行为：当进程持有文件 lease 时，另一个进程执行冲突的 `open(2)` 或 `truncate(2)`，内核需要通知 lease holder，默认信号为 `SIGIO`。holder 收到信号后会按访问类型降级或释放 lease。

## 现象

初始 `log.ans` 中 musl 轮次在 setup 阶段失败：

```text
fcntl33.c:81: TBROK: Failed to open FILE '/proc/sys/fs/lease-break-time' for reading: ENOENT (2)
```

glibc 轮次能够进入主体测试，但所有 lease break 等待都收不到信号：

```text
fcntl33.c:122: TFAIL: failed to receive SIGIO within 5s: EAGAIN/EWOULDBLOCK (11)
```

同时 `truncate(file, 0)` 子项出现过 `ENOENT`，说明相对路径没有按当前工作目录解析。

## 分析

对照 `fcntl33.c` 后确认测试依赖三点语义：

1. `/proc/sys/fs/lease-break-time` 可读写，用于保存和设置 lease break 超时时间。
2. 冲突 `open/truncate` 需要向 lease holder 投递 `SIGIO`。
3. 写 lease 只有被只读 open 打断时才允许降级为读 lease；写打开、读写打开和 truncate 打断时必须释放 lease。

内核已有 `F_SETLEASE/F_GETLEASE` 的最小 lease 表，但只维护互斥状态，没有在 `open/truncate` 冲突路径发 lease break 通知。`sys_truncate()` 还直接把用户路径传给底层 `open()`，对 `"file"` 这类相对路径没有执行 `AT_FDCWD` 解析。

修复过程中还发现：如果直接使用 `send_signal_to_thread_group(SIGIO)`，信号虽然能被 `sigtimedwait()` 消费，但该函数会按 `SIGIO` 默认终止语义提前写入进程级 termination 标记，导致测试结束时又被 `SIGIO/SIGPOLL` 判定为 `TBROK`。lease break 通知只需要把信号放进 holder 的 pending 集合，不能提前设置终止状态。

## 根因

- 启动期缺少 `/proc/sys/fs/lease-break-time` 兼容文件。
- file lease 状态表没有记录 break 通知状态，也没有在普通文件 `open/truncate` 冲突时通知 holder。
- lease 降级缺少“本次 break 是否来自写访问”的状态，导致写访问打断写 lease 后仍错误允许 `F_WRLCK -> F_RDLCK`。
- `sys_truncate()` 未把相对路径转换为绝对路径。

## 修复

- 启动期创建 `/proc/sys/fs/lease-break-time`，默认内容为 `45`。
- `file_lock::lease` 为每个 lease 增加 break 状态：
  - `break_write_requested`：记录本次 break 是否来自写访问。
  - `break_notified`：同一轮 break 只通知一次 holder，避免重复 pending 信号。
- 新增 `notify_file_lease_break()`，在冲突时向 holder 主线程投递 pending `SIGIO`，不设置进程级默认终止标记。
- `open()` 在普通文件访问通过基础权限检查后，根据读写模式触发 lease break 通知。
- `sys_truncate()` 按 `AT_FDCWD` 解析相对路径；`sys_ftruncate()` 在 truncate 前通知 lease holder。
- `F_SETLEASE` 中处理写 lease 降级：如果本次 break 来自写访问，`F_WRLCK -> F_RDLCK` 返回 `EAGAIN`，让 holder 走释放 lease 路径。

## 涉及文件

- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `os/src/fs/kernel_fs_ops/open.rs`
- `os/src/syscall/fs/file_lock/lease.rs`
- `os/src/syscall/fs/file_lock/mod.rs`
- `os/src/syscall/fs/space.rs`

## 验证

已执行：

```text
rustfmt os/src/syscall/fs/file_lock/lease.rs os/src/syscall/fs/file_lock/mod.rs os/src/fs/kernel_fs_ops/open.rs os/src/syscall/fs/space.rs os/src/fs/kernel_fs_ops/initfiles.rs
make
timeout 120s make run > log.ans 2>&1
```

结果：

- 默认 LoongArch64 `make` 通过。
- 最新 `log.ans` 中 musl `fcntl33` 7 项均 `TPASS`，Summary 为 `passed 7 failed 0 broken 0`。
- 最新 `log.ans` 中 glibc `fcntl33` 7 项均 `TPASS`，Summary 为 `passed 7 failed 0 broken 0`。
- 构建中仍有既有 `smoltcp` vendor warning，与本次修复无关。
