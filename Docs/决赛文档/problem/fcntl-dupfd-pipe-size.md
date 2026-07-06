# fcntl DUPFD 与 pipe size 兼容性完善

## 背景

在 `fcntl01` 已修复 `F_GETFL/F_SETFL` 后，继续梳理 `fcntl(2)` 的基础命令兼容性。LTP 中 `fcntl02`、`fcntl12`、`fcntl29`、`fcntl30`、`fcntl35`、`fcntl37` 以及部分 pipe/epoll 用例会覆盖 `F_DUPFD*` 和 `F_GETPIPE_SZ/F_SETPIPE_SZ`。

## 现象

原实现仍有几个 Linux 语义不一致点：

- 已关闭 fd 槽在 `sys_fcntl()` 前置检查中返回 `EINVAL`，而 fd 无效应返回 `EBADF`。
- `F_DUPFD` 直接克隆 `FileDescriptor`，会继承源 fd 的 `FD_CLOEXEC`；Linux 语义下 `F_DUPFD` 新 fd 不应带 `FD_CLOEXEC`，只有 `F_DUPFD_CLOEXEC` 才设置该位。
- `alloc_fd_larger_than()` 对 `arg >= RLIMIT_NOFILE` 和 fd 表已满的错误码没有区分，且在 fd 表长度达到 soft limit 但内部存在空洞时会提前返回 `EMFILE`。
- `F_GETPIPE_SZ` 固定返回 `65536`，没有检查 fd 是否为 pipe；`F_SETPIPE_SZ` 固定返回 `EINVAL`，会导致依赖 pipe 容量调整的用例失败。
- 启动期缺少 `/proc/sys/fs/pipe-max-size`，LTP `fcntl30/fcntl37` 读取 sysctl 前置条件时会失败。

## 分析

`FdTable::get()` 已经能把越界或空槽统一映射为 `EBADF`，因此 `sys_fcntl()` 不应额外把 `try_get(None)` 改写为 `EINVAL`。

`F_DUPFD` 和 `dup(2)` 一样复制 open file description，但新 fd descriptor flag 独立，`FD_CLOEXEC` 不继承；`F_DUPFD_CLOEXEC` 则在新 fd 上设置 close-on-exec。

pipe 当前实现使用 `PipeRingBuffer` 记录已占用字节数，原先容量是硬编码常量。为了支持 `F_SETPIPE_SZ(0)` 缩到一页、按页向上取整、缩小到已占用数据以下返回 `EBUSY`，需要把容量改成 per-pipe 字段。

## 根因

`fcntl` 早期实现以“能跑通基础 fd 操作”为目标，多个命令使用 stub 或固定值，没有区分 fd descriptor flag、file status flag 和 pipe 对象状态；fd 分配 helper 也没有按 `F_DUPFD` 的 Linux errno 规则处理 limit 边界。

## 修复

- `FdTable::alloc_fd_larger_than()`：
  - `arg >= soft_limit` 返回 `EINVAL`。
  - 先扫描 `arg..` 范围内的空槽，允许复用 fd 表中的空洞。
  - 只有确实需要扩展且已达 soft limit 时返回 `EMFILE`。
- `sys_fcntl()`：
  - 使用 `fd_table.get(fd)?` 校验 fd，关闭槽返回 `EBADF`。
  - `F_DUPFD` 克隆后清除 `FD_CLOEXEC`，`F_DUPFD_CLOEXEC` 克隆后设置 `FD_CLOEXEC`。
  - `F_GETPIPE_SZ/F_SETPIPE_SZ` 先确认 fd 是 pipe，再读写 pipe 当前容量。
- `PipeRingBuffer`：
  - 增加 `capacity` 字段，默认 `65536`。
  - `available_write()` 改为按当前容量计算。
  - `Pipe::set_capacity()` 支持 `0` 归一到 `PAGE_SIZE`、按页向上取整、超过 `PIPE_MAX_SIZE` 返回 `EPERM`、小于已占用字节数返回 `EBUSY`。
- 启动期 `/proc/sys/fs/pipe-max-size` 写入当前 `PIPE_MAX_SIZE`，供 LTP sysctl 前置检查读取。

## 涉及文件

- `os/src/fs/fstruct.rs`
- `os/src/fs/files/pipe/mod.rs`
- `os/src/fs/files/pipe/ring_buffer.rs`
- `os/src/fs/mod.rs`
- `os/src/syscall/fs/fd_ops.rs`
- `os/src/fs/kernel_fs_ops/initfiles.rs`

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml
make
/bin/bash -lc 'timeout 120s make run > /tmp/fcntl-improve.log 2>&1'
```

结果：

- `cargo fmt` 通过。
- 默认 LoongArch64 `make` 通过。
- `make run` 未进入 LTP 测试阶段，启动时 ext4 mount 失败并 panic：

```text
ext4_mount: rc = 95
Panicked at crates/lwext4_rust/src/blockdev.rs:103 Failed to mount the ext4 file system
```

该 panic 出现在 `fs::init()` 挂载根文件系统阶段，早于本次修改的 `fcntl` 和启动期 proc/sys 文件创建逻辑；本次未获得 `fcntl01/fcntl30/fcntl37` 的运行结果。
