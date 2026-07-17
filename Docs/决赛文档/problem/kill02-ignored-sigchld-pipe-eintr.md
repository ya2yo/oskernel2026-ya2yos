# kill02 默认忽略 SIGCHLD 打断 pipe 读导致 glibc TBROK

## 背景

`user/src/bin/ltp/mod.rs` 的 `fork_run_ltp_and_collect()` 通过 pipe 收集子进程的标准输出和错误输出。initproc 在读端阻塞读取测试结果，随后等待测试子进程退出。

## 现象

原始 `log.ans` 中，musl `kill02` 已通过：

```text
RUN LTP CASE kill02
kill02 1 TPASS ...
FAIL LTP CASE kill02 : 512
Summary: passed 1 failed 0 broken 0
```

glibc 单跑同一测试则只有包装器的异常退出：

```text
RUN GLIBC LTP SINGLE CASE kill02
RESULT GLIBC LTP SINGLE CASE kill02 : 512
Summary: passed 0 failed 0 broken 1
```

`512` 是 `exit(2) << 8`，对应 LTP 的 `TBROK` 状态。调试日志还显示 glibc 在写回两条测试结果时收到 `SIGPIPE`，随后以退出码 2 结束。

## 分析

最初需排除 `kill(0, SIGUSR1)` 的进程组投递、glibc `sigaction` ABI 和 signal frame 等路径。结果表明，`SIGPIPE` 发生在测试结论写入 stdout 时：收集输出的 pipe 读端已被 initproc 提前关闭。

测试子进程退出时，initproc 收到默认 disposition 为 Ignore 的 `SIGCHLD`。pipe 阻塞读路径只要看到任意未屏蔽 pending signal 就返回 `EINTR`。用户态收集器将 `read() <= 0` 视为 EOF 并关闭读端；glibc `kill02` 后续写入 LTP 输出时发现没有读端，内核返回 `EPIPE` 并投递 `SIGPIPE`。LTP 的 `tst_sig(FORK, DEF_HANDLER, ...)` 把该信号报告为 unexpected signal，最终产生 `TBROK`。

release 构建中该时序可稳定复现；debug 构建改变调度时序后会掩盖该问题。

## 根因

阻塞 pipe 的 read、write 和 readiness wait 将默认忽略或显式 `SIG_IGN` 的 pending signal 错误视作 syscall 中断。Linux 语义中，这类信号不应令阻塞系统调用返回 `EINTR`；真正可见的用户 handler、停止或终止信号仍应保留原有中断和 trap return 分发行为。

## 修复

在 `os/src/signal/pending.rs` 新增 `consume_ignorable_pending_signal_for_current_task()`：它仅消费当前最低编号、未被 mask 且满足以下任一条件的 pending signal：

- disposition 为显式 `SIG_IGN`；
- 没有用户 handler，且默认动作是 `SigOp::Ignore`。

消费时同步清除对应 `sig_pending_info`。`Pipe::read()`、`Pipe::write()`、`wait_readable()` 和 `wait_writable()` 在决定返回 `EINTR` 前调用该 helper；消费成功后重新检查 pipe 状态并继续等待。非忽略信号不被消费，继续按照原逻辑返回 `EINTR`，由 trap return 建立 handler frame 或执行默认动作。

## 涉及文件

- `os/src/signal/pending.rs`
- `os/src/fs/files/pipe/file_impl.rs`
- `os/src/fs/files/pipe/wait.rs`

## 验证

```text
RISC-V:    musl/glibc kill02 均为 passed 2 failed 0 broken 0
LoongArch: musl/glibc kill02 均为 passed 2 failed 0 broken 0
```

两架构 release 构建均通过；仅保留仓库既有 vendored `smoltcp` warning。两次 QEMU 单测均正常 `shutdown!`。
