# fcntl23 文件租约 F_SETLEASE/F_GETLEASE 修复

## 背景

LTP `fcntl23` 覆盖 `fcntl(2)` 的 file lease 基础路径：

- 用 `open(fname, O_RDONLY | O_CREAT, 0777)` 打开普通文件。
- 调用 `fcntl(fd, F_SETLEASE, F_RDLCK)` 设置读租约。
- 调用 `fcntl(fd, F_GETLEASE)` 确认返回 `F_RDLCK`。
- 调用 `fcntl(fd, F_SETLEASE, F_UNLCK)` 释放租约。

该用例只要求基础租约设置、查询和释放语义，不覆盖 lease breaker 的阻塞与 `SIGIO` 通知完整机制。

## 现象

修复前的 `log.ans` 中，musl 与 glibc 两轮 `fcntl23` 都在第一步设置读租约失败：

```text
fcntl23     1  TFAIL  :  fcntl23.c:140: fcntl(tfile_2, F_SETLEASE, F_RDLCK) Failed, errno=11 : Resource temporarily unavailable
FAIL LTP CASE fcntl23 : 256
Summary:
passed   0
failed   1
broken   0
skipped  0
warnings 0

fcntl23     1  TFAIL  :  fcntl23.c:140: fcntl(tfile_3, F_SETLEASE, F_RDLCK) Failed, errno=11 : Resource temporarily unavailable
RESULT GLIBC LTP SINGLE CASE fcntl23 : 256
Summary:
passed   0
failed   1
broken   0
skipped  0
warnings 0
```

日志中没有 panic 或 `TBROK`，失败点集中在 `F_SETLEASE` 返回 `EAGAIN`。

## 分析

对照 `fcntl23.c`，测试文件是只读打开的普通文件，不存在其它进程打开冲突，也没有 lease breaker 场景。当前内核的 `sys_fcntl()` 对 lease 命令仍是临时 stub：

- `F_SETLEASE` 固定返回 `EAGAIN`。
- `F_GETLEASE` 固定返回 `F_UNLCK`。

因此即使最简单的无冲突读租约设置也会被报告为资源暂不可用。

同一组 LTP 后续还会覆盖 `fcntl25/fcntl26` 的写租约基础路径，以及 `fcntl27` 对可写 fd 设置读租约应返回 `EAGAIN` 的场景。因此修复不能只让 `fcntl23` 硬编码成功，需要保留基本参数校验和租约状态查询。

## 根因

Ya2yOS 缺少 file lease 状态管理，`F_SETLEASE/F_GETLEASE` 只是占位实现，未保存当前进程在文件上的租约类型，也没有在 close/exit 时释放租约。

## 修复

- 在 `os/src/syscall/fs/file_lock.rs` 增加 `FILE_LEASES` 全局表，按 inode path 保存 `pid -> lease type`。
- 新增 `set_file_lease()`：
  - 只接受 `F_RDLCK/F_WRLCK/F_UNLCK`，其它参数返回 `EINVAL`。
  - `F_RDLCK` 遇到可写打开 fd 返回 `EAGAIN`，兼容 `fcntl27` 期望。
  - 不同进程之间写租约与任意租约互斥，多个读租约可共存。
  - 同一进程重复设置租约时更新原租约类型。
  - `F_UNLCK` 移除当前进程在该路径上的租约。
- 新增 `get_file_lease()`，返回当前进程在该路径上的租约类型，没有租约时返回 `F_UNLCK`。
- `sys_fcntl()` 的 `F_SETLEASE/F_GETLEASE` 分支改为解析普通文件 fd、读取 fd 打开模式并委托给租约表。
- `close()`、`close_range()` 和进程最终退出路径增加 file lease 清理，避免租约残留污染后续测试。

说明：本次实现的是 LTP 基础 lease 用例所需的最小兼容层，尚未实现 lease breaker 阻塞、超时降级和 `SIGIO` 通知完整机制。

## 涉及文件

- `os/src/syscall/fs/file_lock.rs`
- `os/src/syscall/fs/fd_ops.rs`
- `os/src/task/mod.rs`

## 验证

已执行：

```text
make
timeout 120s make run > /tmp/fcntl23-lease-fix.log 2>&1
```

结果：

- 默认 LoongArch64 `make` 通过。
- `make run` 需要 QEMU 在 `/var/tmp` 创建临时文件，首次沙箱内运行失败；随后使用非沙箱权限重跑成功。
- `/tmp/fcntl23-lease-fix.log` 中 musl 与 glibc 两轮 `fcntl23` 均通过：

```text
fcntl23     1  TPASS  :  fcntl(tfile_2, F_SETLEASE, F_RDLCK)
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0

fcntl23     1  TPASS  :  fcntl(tfile_3, F_SETLEASE, F_RDLCK)
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```
