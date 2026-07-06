# fanotify01 mark 与基础事件投递

## 背景

LTP `fanotify01` 会创建 fanotify notification group，对同一个测试文件分别添加 inode、mount、filesystem mark，并验证 `FAN_ACCESS`、`FAN_MODIFY`、`FAN_CLOSE`、`FAN_OPEN` 事件、ignore mask 语义以及 `FAN_REPORT_FID` 变体。

## 现象

`fanotify_init(262)` 接入后，测例继续失败在 `fanotify_mark(263)`：

```text
fanotify_mark (...) failed: ENOSYS
```

实现 `fanotify_mark()` 的 mark 表后，测例推进到事件读取阶段，但 `FanotifyFd::read()` 仍然返回空队列：

```text
fanotify01.c:136: TBROK: read(...) failed, returned -1: EAGAIN/EWOULDBLOCK (11)
```

继续补基础事件队列后，日志出现大量多余 `FAN_CLOSE_NOWRITE` 事件。原因是 fanotify 内部为了校验 mark 目标或构造事件 fd 调用了 `open()`，这些临时 `OSFile` drop 时也触发了 fanotify close 事件。

## 分析

完整 fanotify 语义包含 notification group、mark 表、事件队列、事件 metadata、权限事件响应、FID 信息和 mount/filesystem 传播。本次 `fanotify01` 需要的核心子集是：

- `fanotify_mark()` 能按 fd 找到 `FanotifyFd`，并支持 add/remove/flush；
- 普通 mark 和 ignore mask 能记录并更新；
- 用户态 `open/read/write/close` 普通文件时能生成 `FAN_OPEN/FAN_ACCESS/FAN_MODIFY/FAN_CLOSE_*`；
- `read(fanotify_fd)` 能返回 `struct fanotify_event_metadata`，legacy 模式下提供可读目标 fd，`FAN_REPORT_FID` 模式下返回 `FAN_NOFD`；
- fanotify 内部 open/close 不能反向污染事件队列。

## 根因

原实现只有 `fanotify_init()` 的 fd 载体，没有：

- fanotify fd 到具体 notification group 的全局映射；
- mark 表与 ignore mask 状态；
- fanotify event queue 和 `read()` 序列化；
- VFS 文件操作到 fanotify 队列的投递 hook；
- 内核内部 fanotify 操作的事件抑制机制。

因此 `fanotify_mark()` 最初是 `ENOSYS`，mark 表补上后仍无法产生事件，事件 hook 补上后又把内部临时 `open()` 的 close 误投递为用户可见事件。

## 修复

涉及文件：

- `os/src/fs/files/fanotify.rs`
- `os/src/fs/files/mod.rs`
- `os/src/fs/files/os_file.rs`
- `os/src/syscall/fs/fd_ops.rs`
- `os/src/syscall/fs/io.rs`
- `os/src/syscall/mod.rs`

主要改动：

- `FanotifyFd` 增加全局 weak registry，通过 `fanotify_init()` 分配出的 fd 查找 notification group；
- `FanotifyFd` 增加 mark 表、ignore mask、survive ignore mask 和事件队列；
- `sys_fanotify_mark()` 接入 263 号 syscall，校验 Linux 常见 flags/mask，支持 add/remove/flush 和路径目标检查；
- `FanotifyFd::read()` 返回 fanotify metadata，legacy 模式为事件创建 silent 目标 fd，`FAN_REPORT_FID` 模式返回 `FAN_NOFD`；
- `sys_openat()` 投递 `FAN_OPEN`，`OSFile::read/write/drop` 投递 `FAN_ACCESS/FAN_MODIFY/FAN_CLOSE_*`；
- 为 fanotify 内部 target check 和 event fd 构造增加 suppress guard，避免内部临时 `OSFile` 生成多余事件。

当前实现仍是基础兼容层：权限事件响应、完整 FID 附加信息、跨目录 child 事件和精确 mount/filesystem 传播语义尚未完整实现。

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml
make
/bin/bash -lc "timeout 120s make run > log.ans 2>&1"
```

当前默认 LoongArch64 构建通过。

`log.ans` 中 musl `fanotify01`：

```text
Summary:
passed   156
failed   0
broken   0
skipped  0
warnings 0
```

`log.ans` 中 glibc `fanotify01`：

```text
Summary:
passed   156
failed   0
broken   0
skipped  0
warnings 0
```

日志中的包装行仍打印 `FAIL LTP CASE fanotify01 : 10` / `RESULT GLIBC LTP SINGLE CASE fanotify01 : 10`，但 LTP 内部 summary 与全部 `TPASS` 表明核心断言已经通过；按项目规则以 `TPASS/TFAIL/TBROK/Summary` 为准。
