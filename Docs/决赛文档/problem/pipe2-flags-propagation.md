# pipe2 flags 丢失导致 F_GETFD/F_GETFL 失败

## 背景

Linux `pipe2(int pipefd[2], int flags)` 支持在创建匿名 pipe 时设置 `O_CLOEXEC`、`O_NONBLOCK` 和 `O_DIRECT`。其中：

- `O_CLOEXEC` 是两个 fd 各自的 descriptor flag，`F_GETFD` 应看到 `FD_CLOEXEC`；
- `O_NONBLOCK` 是两个端点的 file status flag，`F_GETFL` 应看到该位，实际读写也不得再阻塞；
- `O_DIRECT` 使 pipe 进入 packet mode，Linux 仅在写端的 `F_GETFL` 中显示该位。

LTP `pipe2_01` 覆盖这些 flag 的可见性。

## 现象

新的 `log.ans` 中，musl 与 glibc 均有相同 5 项失败：

```text
TFAIL: pipe2 fds[0] doesn't get expected flag(524288), get flag(0)
TFAIL: pipe2 fds[1] doesn't get expected flag(524288), get flag(0)
TFAIL: pipe2 fds[1] doesn't get expected flag(16384), get flag(0)
TFAIL: pipe2 fds[0] doesn't get expected flag(2048), get flag(0)
TFAIL: pipe2 fds[1] doesn't get expected flag(2048), get flag(0)

Summary:
passed   2
failed   5
broken   0
```

`0` flag 的两个检查通过，表明 pipe 创建本身正常，失败只发生在创建 flags 的保存与查询。

## 分析

系统调用分发原来调用：

```rust
Syscall::Pipe2 => sys_pipe2(args[0] as *mut u32)
```

第二个 ABI 参数 `args[1]` 被直接丢弃。`sys_pipe2()` 也没有 flags 参数，并始终用 `FileDescriptor::default()` 创建两个端点，因此 fd descriptor flags 和 file status flags 均为零。

这使 `F_GETFD` 看不到 `O_CLOEXEC` 产生的 `FD_CLOEXEC`，`F_GETFL` 也看不到 `O_NONBLOCK` 或写端的 `O_DIRECT`。此外，即使仅修复 fd 表 flags，`Pipe::read()`/`write()` 仍会读取端点的 `AtomicBool` 非阻塞状态，因此 `O_NONBLOCK` 还需要同步到两个 `Pipe` 对象。

## 根因

`pipe2` 入口错误地按旧 `pipe()` 的无 flags 形式实现，导致 Linux ABI 的第二个参数从分发层到 fd 创建层完全丢失；不存在 flag 合法性校验或端点状态初始化。

## 修复

涉及文件：

- `os/src/syscall/mod.rs`
- `os/src/syscall/fs/pipe.rs`

主要修改：

- syscall 分发把 `args[1]` 传递给 `sys_pipe2(fd, flags)`；
- 仅接受 `O_CLOEXEC | O_NONBLOCK | O_DIRECT`，未知位返回 `EINVAL`；
- 两个端点均保存 `O_CLOEXEC` 和 `O_NONBLOCK`，使 `F_GETFD`/`F_GETFL` 结果正确；
- 创建时将 `O_NONBLOCK` 同步到读写两个 `Pipe` 的实际等待状态；
- `O_DIRECT` 仅保存到写端 fd，保持 Linux 的 `F_GETFL` 可见性。

当前 `Pipe` 的缓冲区尚未实现 `O_DIRECT` 所需的完整 packet framing 和短读丢弃语义。本次修复的范围是 `pipe2_01` 所验证的 flag ABI、fd 状态与 nonblocking 行为，不将 `O_DIRECT` 标记可见性等同于完整 packet-mode 实现。

## 验证

已执行：

```text
rustfmt --edition 2021 --check os/src/syscall/fs/pipe.rs os/src/syscall/mod.rs
make
timeout 180s make run > /tmp/pipe2_01-fix.log 2>&1
```

结果：

- `rustfmt --check` 与 `git diff --check` 通过；
- `make` 完成 RISC-V、LoongArch64 两个架构构建，仅有既有 vendored `smoltcp` warning；
- RISC-V QEMU 下 musl 与 glibc `pipe2_01` 的所有 7 项断言均通过：

```text
TPASS: pipe2 fds[0] gets expected flag(524288)
TPASS: pipe2 fds[1] gets expected flag(524288)
TPASS: pipe2 fds[1] gets expected flag(16384)
TPASS: pipe2 fds[0] gets expected flag(2048)
TPASS: pipe2 fds[1] gets expected flag(2048)

Summary:
passed   7
failed   0
broken   0
skipped  0
warnings 0
...
shutdown!
```

`FAIL LTP CASE pipe2_01 : 0` 是 musl 单测包装器对成功退出码的既有输出，按 LTP Summary 判定为通过。
