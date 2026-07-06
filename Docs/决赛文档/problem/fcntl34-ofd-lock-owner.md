# fcntl34 OFD lock owner 语义修复

## 背景

LTP `fcntl34` 验证 `F_OFD_SETLKW` 是否能在同一进程的多个线程之间同步文件写入。测试中每个线程独立 `open("tst_ofd_locks", O_RDWR)`，然后用 OFD write lock 保护 `lseek(fd, 0, SEEK_END)` 和 `write(fd, 4096)`，最后主线程读取文件并校验每个线程写入的块数量与内容。

## 现象

新的 `log.ans` 中 musl/glibc `fcntl34` 都在校验阶段失败：

```text
fcntl34.c:99: TBROK: read(...) failed, returned 0
```

说明多线程写入后文件内容不符合预期，主线程在读取固定数量的 4096 字节块时提前遇到 EOF。

## 分析

`fcntl34.c` 使用的是 OFD lock，而不是 POSIX process-associated record lock。OFD lock 的 owner 应该是 open file description：同一次 `open()` 创建一个 owner，`dup()` 出来的 fd 共享 owner；不同 `open()` 即使在同一进程中也应该互相冲突。

内核原实现把 OFD lock 简化委托给 POSIX lock 逻辑，但仍使用当前进程 pid 作为 owner。因此 `fcntl34` 中多个线程虽然分别 `open()` 了不同 fd，但 owner 都是同一个 pid，锁层认为它们是同一 owner，不发生互斥。这样 `lseek(SEEK_END)` 和 `write()` 之间会被其他线程穿插，多个 fd 可能写到同一旧 EOF 位置，最终文件缺块或内容被覆盖。

同时，`F_OFD_SETLKW` 原先和 `F_OFD_SETLK` 走同一条非阻塞 `setlk()` 路径；真正遇到冲突时也不会按 `SETLKW` 语义等待。

## 根因

- OFD lock 错误复用进程 pid 作为 owner，导致同一进程内不同 open file description 不互斥。
- `F_OFD_SETLKW` 没有走阻塞等待路径。
- 关闭 fd 时没有针对 OFD owner 释放对应锁，容易留下后续冲突状态。

## 修复

- `OSFile` 新增稳定的 `ofd_lock_owner`：
  - 每次 `OSFile::new()` 分配一个新的 owner id。
  - owner id 使用负数，避免和正数 pid owner 混淆。
  - `dup()` 共享同一个 `Arc<OSFile>`，因此自然共享同一个 OFD owner。
- `F_OFD_GETLK/F_OFD_SETLK/F_OFD_SETLKW` 改用 `osfile.ofd_lock_owner()` 作为锁 owner。
- `F_OFD_SETLKW` 改走已有 `setlk_blocking()`，支持等待冲突锁释放。
- `F_OFD_GETLK` 在发现冲突时将 `l_pid` 写为 `-1`，贴近 Linux OFD lock 不关联进程 pid 的语义。
- `close/close_range` 在最后一个 fd 引用关闭时释放该 open file description 对应的 OFD 锁。

## 涉及文件

- `os/src/fs/files/os_file.rs`
- `os/src/syscall/fs/fcntl.rs`
- `os/src/syscall/fs/fd_ops.rs`

## 验证

已执行：

```text
rustfmt os/src/fs/files/os_file.rs os/src/syscall/fs/fcntl.rs os/src/syscall/fs/fd_ops.rs
make
timeout 120s make run > log.ans 2>&1
```

结果：

- 默认 LoongArch64 `make` 通过。
- 最新 `log.ans` 中 musl `fcntl34` 为 `TPASS`，Summary 为 `passed 1 failed 0 broken 0`。
- 最新 `log.ans` 中 glibc `fcntl34` 为 `TPASS`，Summary 为 `passed 1 failed 0 broken 0`。
- 构建中仍有既有 `smoltcp` vendor warning，与本次修复无关。
