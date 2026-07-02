# splice syscall 兼容实现

## 背景

`splice(2)` 是 LTP 与比赛测试会覆盖的文件/pipe 数据搬运 syscall。仓库已经接入 Linux syscall 号 76，但 `sys_splice()` 仍停留在未实现状态，当前工作树中还存在未完成的 `let ibuf = in_pipe.` 代码片段，导致代码不可编译。

## 现象

- `sys_splice()` 只做部分 fd 与 offset 参数检查后返回 `EINVAL`，不能通过 `splice01` 的 regular file -> pipe -> regular file 数据回读验证。
- 未完成代码会破坏构建。
- offset 指针、pipe offset、`O_APPEND` 输出 fd、无 pipe 参与、非阻塞 pipe 等 Linux 可见语义缺失。

## 分析

Ya2yOS 当前没有页缓存级零拷贝 splice 机制，但已有 `File::read()` / `File::write()`、`UserBuffer`、`OSFile::lseek()` 和 `Pipe` ring buffer 抽象，足以实现兼容搬运路径。为了保持 syscall 层薄且不侵入底层文件系统，本次选择通过固定大小内核缓冲区分片搬运。

关键语义：

- `fd_in` 和 `fd_out` 至少一个必须是 pipe，否则返回 `EINVAL`。
- pipe 端不接受 offset 指针，传入非空 offset 返回 `ESPIPE`。
- 非 pipe 端传入 offset 指针时，临时 `lseek()` 到用户 offset，读写完成后恢复 fd 原 offset，并只把实际搬运字节数写回用户 offset。
- 输出 fd 带 `O_APPEND` 时返回 `EINVAL`。
- `SPLICE_F_NONBLOCK` 在 pipe 无可读/可写空间时返回 `EAGAIN`；已搬运部分数据后遇到错误则返回已搬运字节数。

## 根因

原实现把 `splice` 保留为占位 syscall，没有真正连接现有 `File`/`Pipe` 数据路径，也没有完整实现 Linux 的参数校验与 offset 更新规则。

## 修复

- 在 `os/src/syscall/io_mpx/splice.rs` 中实现 `sys_splice()`：
  - 校验 flags、fd、`O_PATH`、pipe 参与、pipe offset、`O_APPEND`。
  - 使用 64KiB 内核缓冲区分片执行 `read()` -> `write()`。
  - 对带 offset 的普通文件读写执行临时 seek、恢复 fd offset、收尾写回用户 offset。
  - 支持 file->pipe、pipe->file、pipe->pipe 的基础搬运。
  - 处理 pipe 读写端关闭、非阻塞空/满 pipe 和短读短写。
- 在 `os/src/fs/files/pipe.rs` 中公开 pipe 读端/写端关闭状态查询，供 `SPLICE_F_NONBLOCK` 和 `EPIPE` 判断使用。
- 在 `os/src/syscall/mod.rs` 中把 `splice` 的 offset 参数按可写用户指针传入。

## 验证

已执行：

```text
rustfmt --edition 2024 --unstable-features --skip-children os/src/syscall/io_mpx/splice.rs os/src/syscall/mod.rs os/src/fs/files/pipe.rs
make
make TARGET_ARCH=riscv64
timeout 120s make run > /tmp/splice-run.log 2>&1
git diff --check
```

结果：

- 默认 LoongArch64 `make` 通过。
- `make TARGET_ARCH=riscv64` 通过。
- LoongArch64 `make run` 使用当前 `initproc` 的 `splice01` 单测入口，musl/glibc 均输出 `splice01.c:48: TPASS: Written data has been read back correctly`，summary 均为 `passed 1 failed 0 broken 0 skipped 0 warnings 0`，系统跑到 `shutdown!`。
- 构建仍有 vendor `smoltcp` warning，与本次修改无关。
- LoongArch64 glibc 运行日志仍出现一行既有 `Fail to convert LoongArch Unknown to Trap type! 0x0`，但未导致 `TFAIL` / `TBROK` / panic，`splice01` summary 通过。
