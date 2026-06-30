# kill11: wait status core dump bit 编码错误

## 背景

LTP `kill11` 会逐个向子进程发送一组信号，然后父进程通过 `wait()` 获取 `status`，检查：

- `WTERMSIG(status)` 是否等于发送的信号；
- `WCOREDUMP(status)` 是否只对默认 core dump 信号置位。

其中 SIGHUP、SIGINT、SIGKILL、SIGUSR1、SIGTERM 等默认动作是终止但不 core dump；SIGQUIT、SIGILL、SIGABRT、SIGSEGV、SIGXCPU、SIGSYS 等默认动作会 core dump。

## 现象

修复前 `log.ans` 中，真正 core dump 的信号能通过，但所有默认终止类信号也被错误设置了 core dump bit：

```text
kill11.c:88: TFAIL: core dump bit set for SIGHUP
kill11.c:88: TFAIL: core dump bit set for SIGINT
kill11.c:88: TFAIL: core dump bit set for SIGKILL
kill11.c:88: TFAIL: core dump bit set for SIGUSR1
...
Summary:
passed   11
failed   13
broken   0
```

musl 和 glibc 单测现象一致。

## 分析

信号默认动作表本身是正确的：

- `SigOp::Terminate` 包含 SIGHUP、SIGINT、SIGKILL、SIGUSR1、SIGTERM 等；
- `SigOp::CoreDump` 包含 SIGQUIT、SIGILL、SIGABRT、SIGSEGV、SIGXCPU、SIGSYS 等。

默认信号终止时，进程元数据中也会记录：

```text
termination_signal = Some((signo, dumped_core))
```

问题出在 `sys_waitpid()` 写回 `wstatus` 的编码。旧逻辑只看 `exit_code`：

```text
if exit_code >= 128 && exit_code <= 255 {
    value = exit_code;
} else {
    value = exit_code << 8;
}
```

而信号终止路径会以 `signo + 128` 作为内部退出码。这个值低 8 位天然包含 `0x80`，用户态 `WCOREDUMP(status)` 会把所有信号终止都识别为 core dump，即使该信号只是普通 terminate。

## 根因

`wait()/waitpid()` 没有使用已经记录的 `termination_signal` 元数据，而是把内部 `exit_code = signo + 128` 当成用户可见 wait status 返回。`signo + 128` 会无条件带上 `WCOREDUMP` 的 0x80 位，导致非 core 信号也被报告为 core dump。

## 修复

在 `os/src/syscall/task/wait.rs` 中增加 wait status 编码 helper：

```text
if termination_signal == Some((signo, dumped_core)) {
    status = signo | (dumped_core ? 0x80 : 0);
} else {
    status = exit_code << 8;
}
```

`sys_waitpid()` 回收已退出子进程时同时读取 `child_usage` 和 `termination_signal`，并用该 helper 写回 `wstatus`。

涉及文件：

- `os/src/syscall/task/wait.rs`

## 验证

已执行：

```text
rustfmt os/src/syscall/task/wait.rs
make
timeout 120s make run > log.ans 2>&1
```

当前默认 `TARGET_ARCH=loongarch64`，`make` 通过。

复现配置下单跑 musl/glibc `kill11`，两者均通过 LTP Summary：

```text
Summary:
passed   24
failed   0
broken   0
skipped  0
warnings 0
```

`log.ans` 中未再出现 `core dump bit set for ...`。外层仍打印 `FAIL LTP CASE kill11 : 10` / `RESULT ... : 10`，但 LTP 内部 Summary 已经全 TPASS；该 wrapper 行不是本次断言失败。
