# splice07 文件类型校验与空 pipe 卡死

## 背景

`splice07` 覆盖多类 fd 之间的 `splice(2)` 错误路径。测试重点不是搬运成功，而是确认不支持 splice 的 fd 组合能在进入阻塞 I/O 前返回 Linux 兼容的 `EINVAL` 或 `EBADF`。

## 现象

用户提供的 `log.ans` 显示 `splice07` 在前半段已经大量 `TPASS`，但存在两个真实问题：

- `directory -> pipe write end` 返回成功，LTP 输出 `TFAIL: splice() on directory -> pipe write end succeeded`。
- 最后停在 `pipe read end -> /dev/zero`，日志最后一条 syscall 为 `splice(fd_in=0, fd_out=/dev/zero, len=1)`，随后 QEMU 被超时终止。

## 分析

`sys_splice()` 原实现只检查“至少一端是 pipe”、读写权限、`O_PATH`、pipe offset、`O_APPEND` 等条件。对于非 pipe 端，它直接进入通用 `read()` -> `write()` 搬运路径。

这会带来两个错误：

- 目录 fd 作为输入端时，只要 `readable()` 为真，就可能进入普通读路径，导致 `directory -> pipe write end` 错误成功。
- `/dev/zero` 作为输出端时，`DevZero::writable()` 为真，`pipe read end -> /dev/zero` 被误认为可搬运。输入 pipe 此时为空且写端未关闭，于是 `Pipe::read()` 按阻塞语义挂起，形成卡死。

修复后首次验证又暴露出 `pipe read end -> signalfd` 的 panic：未实现 fd 由 `DummyFd` 占位，`DummyFd` 未实现 `fstat()`，而新增类型校验需要读取 `st_mode`。这类 fd 应作为不支持 splice 的对象返回 `EINVAL`，不应触发默认 `File::fstat()` panic。

## 根因

`splice` 的非 pipe 端缺少基于 `fstat().st_mode` 的文件类型准入检查，把“可读/可写”误等同于“支持 splice”。同时 `DummyFd` 作为未实现 fd 占位对象缺少基础 `fstat()`，导致错误路径检查期间 panic。

## 修复

- 在 `os/src/syscall/io_mpx/splice.rs` 中新增局部类型校验：
  - 非 pipe 输入端只允许 `FREG` 和 `FCHR`，保留 `/dev/zero -> pipe` 这类 LTP 跳过但 Linux 可成功的路径。
  - 非 pipe 输出端只允许 `FREG`，提前拒绝 `/dev/zero`、eventfd、inotify、DummyFd 等输出端。
  - 校验放在读写权限检查之后、实际 `read()` 前，保持 `O_PATH` 和只读输出 fd 的 `EBADF` 优先级。
- 在 `os/src/fs/files/dummyfd.rs` 中为 `DummyFd` 补 `fstat() -> Kstat::default()`，使未实现 fd 在 `splice` 类型检查中稳定落到 `EINVAL`，避免默认 trait panic。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/syscall/io_mpx/splice.rs` | 为 `sys_splice()` 的非 pipe 端增加 `st_mode` 类型准入检查 |
| `os/src/fs/files/dummyfd.rs` | 为未实现 fd 占位对象补默认 `fstat()` |

## 验证

已执行：

```text
make
timeout 120s make run > /tmp/splice07-fix.log 2>&1
```

结果：

- 默认 RISC-V `make` 通过，仅有既有 warning。
- `make run` 中 `splice07` 跑到 `shutdown!`，未再卡死或 panic。
- 关键断言已修复：

```text
splice07.c:56: TPASS: splice() on directory -> pipe write end : EINVAL (22)
splice07.c:56: TPASS: splice() on pipe read end -> /dev/zero : EINVAL (22)
splice07.c:56: TPASS: splice() on pipe read end -> signalfd : EINVAL (22)
splice07.c:56: TPASS: splice() on pipe read end -> timerfd : EINVAL (22)

Summary:
passed   566
failed   0
broken   0
skipped  25
warnings 0
```

日志仍包含包装器行 `FAIL LTP CASE splice07 : 10`，但本仓库判读 LTP 时以 `TPASS` / `TFAIL` / `TBROK` / `Summary` 为准；本次 summary 为 `failed 0 broken 0`。

未执行 `TARGET_ARCH=loongarch64` 验证；本次复现与验证基于当前默认 RISC-V 配置。
