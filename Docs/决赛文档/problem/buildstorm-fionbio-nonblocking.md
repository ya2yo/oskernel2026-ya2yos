# BuildStorm Rust 子进程管道 FIONBIO 兼容

## 背景

BuildStorm 的 Cargo/Rust 组件会以 Rust 标准库 `Command::output()` 收集子进程的 stdout 和
stderr。Linux 目标下，标准库会先对两根匿名 pipe 执行 `ioctl(FIONBIO)`，再用 poll/read
并行读取两路输出。

## 现象

动态库路径修复后，BuildStorm 的 Cargo 进程出现：

```text
thread 'main' panicked at .../library/std/src/process.rs:2454:21:
called `Result::unwrap()` on an `Err` value: Os { code: 25, ...
"Inappropriate ioctl for device" }
```

这会使 `cargo build -p tg-xtask` 与后续 `cargo xtask` 在启动阶段失败。

## 分析

`process.rs:2454` 是 `wait_with_output()` 对 `read_output()` 结果的 `unwrap()`。Rust Linux
实现会调用：

```text
ioctl(fd, FIONBIO, &int)
```

其中 RISC-V asm-generic `FIONBIO` 的请求号为 `0x5421`，参数是 4 字节用户态 `int`，非零
表示设置 `O_NONBLOCK`。

旧内核的 `sys_ioctl()` 只把命令转发给具体 `File`。`Pipe::ioctl()` 仅实现 `FIONREAD`
(`0x541b`)，其余请求返回 `ENOTTY`；虽然 `Pipe` 已有 `set_nonblocking()`，该能力无法经
标准 `FIONBIO` ABI 调用。

Linux 7.0 在 `fs/ioctl.c` 的通用 VFS 路径处理 `FIONBIO`：安全读取用户 `int` 后更新
file status flags，而不是让 pipe/tty 驱动各自处理。

## 根因

内核遗漏了 Linux common-VFS `FIONBIO` 语义，使 Rust 标准库无法将子进程输出管道改为
非阻塞，从而把 `ENOTTY` 传播到 `wait_with_output()` 的 panic。

## 修复

`os/src/syscall/fs/ctl/ioctl.rs` 在分派到具体 `File::ioctl()` 前新增通用分支：

1. 用 `copy_from_user_val::<i32>()` 安全读取 `arg` 指向的开关值，坏指针保留 `EFAULT`。
2. 调用 `File::set_nonblocking()` 更新底层对象；pipe 会更新其共享的 atomic 状态。
3. 同步更新 fd table 的 `O_NONBLOCK`，保证 `fcntl(F_GETFL)` 可见。
4. 成功返回 `0`；其他 ioctl 继续原有的具体文件分派。

## 涉及文件

- `os/src/syscall/fs/ctl/ioctl.rs`

## 验证

debug 日志确认修复前 PID 10、PID 14 的 `cmd=21537` 均返回 `ENOTTY`。修复后 RISC-V
release `log.ans` 中不再出现 `Inappropriate ioctl for device` 或 `process.rs:2454` panic，
且 `BUILDSTORM_TOOLCHAIN ok` 已出现。双架构 release 构建通过；完整 guest 编译仍受本机
`2G / 2 CPU` 资源限制，未记录为已通过。
