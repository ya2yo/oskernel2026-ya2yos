# libctest: sigtimedwait 后 wait4 误返回 EINTR

## 背景

运行 libctest 的 `clocale_mbfuncs` 时，`runtest.exe` 负责 fork/exec `entry-static.exe`，再等待子进程结束并汇总测试结果。日志来自 `clocale.ans`。

## 现象

`entry-static.exe clocale_mbfuncs` 实际很快正常退出，但父进程 `runtest.exe` 在等待子进程状态时报错：

```text
SigTimedWait ret = 17
Wait4 ret = Interrupted system call
src/common/runtest.c:91: clocale_mbfuncs waitpid failed: Interrupted system call
FAIL clocale_mbfuncs [internal]
```

因此 clocale 单测以及依赖同一 `runtest.exe` 包装路径的 libctest 测例都会被判为 internal fail。

## 分析

日志中的关键顺序是：

1. `runtest.exe` 为 `SIGCHLD` 设置自定义 handler，并 fork 出 PID 3。
2. PID 2 进入 `rt_sigtimedwait()` 等待 `SIGCHLD`。
3. PID 3 `execve(entry-static.exe)` 后 `exit_group(0)`。
4. 子进程退出向父进程投递 `SIGCHLD`，父进程的 `SigTimedWait ret = 17` 表明该信号已经被 `sigtimedwait` 成功消费。
5. PID 2 随后调用 `wait4(pid=3)`，此时子进程已经是 zombie，本应立即回收并返回 PID 3，却在扫描 child 前被 `interruptible()` 看到内部 `interrupted` 标志而返回 `EINTR`。

此前已有超时路径的类似问题：`sigtimedwait` 超时时，定时器通过 `task.interrupt()` 唤醒等待者，返回 `EAGAIN` 前必须清理内部中断标志。本次失败不是超时，而是收到 `SIGCHLD` 的成功路径。

`add_signal()` 对带自定义 handler 的 `SIGCHLD` 会调用 `wake_interruptible()` 唤醒阻塞任务。该唤醒会设置 `TaskControlBlock::interrupted = true`，用于让真正的 interruptible wait 返回 `EINTR`。但 `sys_rt_sigtimedwait()` 本身使用 `block_on(poll_fn(...))`，成功匹配并消费 pending signal 后直接返回 `Ok(signo)`，没有清理本次注册的 `interrupt_waker` 和 `interrupted` 标志。

于是内部唤醒状态泄漏到了下一个 syscall。`wait4()` 外层使用 `block_on(interruptible(...))`，`interruptible()` 在 poll 子进程状态前先调用 `poll_interrupt()`，读到残留标志后直接返回 `EINTR`。

## 根因

`sys_rt_sigtimedwait()` 只在超时返回 `EAGAIN` 时清理内部 interrupt 状态；成功消费信号返回时没有清理 `interrupt_waker/interrupted`。对 `runtest.exe` 这种 `sigtimedwait(SIGCHLD)` 后立即 `waitpid()` 的流程，唤醒 `sigtimedwait` 的内部中断被错误传播成后续 `wait4()` 的用户可见 `EINTR`。

## 修复

在 `sys_rt_sigtimedwait()` 成功匹配并从 `sig_pending` 中移除目标信号后，调用 `task.clear_interrupt_waiter()`：

- 清除本次等待留下的 `interrupted` 标志。
- 移除本次 `sigtimedwait` 注册的 `interrupt_waker`。
- 保持 `sigtimedwait` 的用户可见返回值仍为实际信号编号，例如 `SIGCHLD(17)`。

这只处理 `sigtimedwait` 自己的内部唤醒状态，不改变 `wait4()` 对真正可见信号返回 `EINTR` 的语义。

## 涉及文件

- `os/src/syscall/signal.rs`

## 验证

已执行：

```text
make
timeout 120s make run > /tmp/clocale-sigtimedwait-fix.log 2>&1
```

结果：

- `make` 在当前默认 RISC-V 配置下通过。
- 第一次 `make run` 在沙箱中因 QEMU 需要写 `/var/tmp` 临时文件失败，按权限流程重新运行后通过。
- `/tmp/clocale-sigtimedwait-fix.log` 中输出：

```text
========== START entry-static.exe clocale_mbfuncs ==========
Pass!
========== END entry-static.exe clocale_mbfuncs ==========
shutdown!
```

- 过滤日志未再出现 `Interrupted system call`、`waitpid failed`、`FAIL` 或 panic。

未执行 `TARGET_ARCH=loongarch64` 验证；本次复现与修复在当前默认 RISC-V 配置下完成。
