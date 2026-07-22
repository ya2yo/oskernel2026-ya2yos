# kill07 SIGKILL 快速退出的 waitpid 状态编码

## 背景

LTP `kill07` 验证 `SIGKILL` 不可捕获，以及父进程通过 `waitpid()` 观察子进程终止时的 Linux ABI。测试先尝试对 `SIGKILL` 安装 handler，预期得到 `EINVAL`；随后子进程 `sleep(300)`，父进程执行 `kill(child, SIGKILL)` 并等待。

测试期望 `WIFSIGNALED(status) != 0` 且 `WTERMSIG(status) == SIGKILL`。这要求内核将 wait status 编码为低 7 位的信号号 `9`，而不是普通 `exit(137)` 的 `137 << 8`。

## 现象

根目录 `log.ans` 的最后一个真实失败为：

```text
kill07      1  TFAIL  :  kill07.c:161: No signals received
```

相关日志证明信号投递和子进程回收都已发生：

- 父进程的 `SigKill` syscall 返回 0。
- 子进程从 `nanosleep(300)` 进入 `exit_current_and_run_next()`。
- 父进程的 `Wait4` 返回子进程 PID。

`No signals received` 不是父进程遗漏 `SIGCHLD`。日志随后显示父进程收到并按默认 ignore disposition 消费了 `SIGCHLD`；该 LTP 分支只表示 `WTERMSIG(status) == 0`。

## 分析

LTP 20240524 的 `kill07.c` 在 `waitpid()` 后执行：

```c
nsig = WTERMSIG(status);
asig = WIFSIGNALED(status);

if ((asig == 1) & (nsig == SIGKILL))
    tst_resm(TPASS, "Did not catch signal as expected");
else if (nsig)
    tst_resm(TFAIL, "expected signal %d received %d", SIGKILL, nsig);
else
    tst_resm(TFAIL, "No signals received");
```

用户态 `kill()` 经过如下路径：

```text
sys_kill
  -> send_user_signal_to_thread_group
  -> add_signal_with_info(SIGKILL, Some(SigInfo))
  -> child nanosleep / block_on
  -> exit_current_if_group_exited_or_killed
  -> exit_current_and_run_next(137)
  -> sys_waitpid
```

普通 trap-return 信号分发在 `signal::handle_signal()` 的 `Terminate/CoreDump` 分支会写入 `ProcessMeta.termination_signal`。但是 `SIGKILL` 唤醒 blocked task 后可能先进入任务调度的快速退出门，绕过该分支。旧实现只以内部退出码 137 回收任务。

`sys_waitpid()` 的 `wait_status_from_exit_code()` 只有在 `termination_signal` 存在时才写入 Linux 信号终止状态；否则统一写入 `exit_code << 8`。因此旧路径把 137 写成 `0x8900`，使 `WIFSIGNALED` 为假、`WTERMSIG` 为 0，精确触发 `kill07` 的失败分支。

回归来自 `2ce578d fix(buildstorm): 修复启动与 rustc 工具链路径` 引入的协作调度 `SIGKILL` 快速退出路径。`block_on()` 还保留了一条独立的同类快速路径，`nanosleep` 正是经此路径复现。

## 根因

阻塞任务被用户态 `SIGKILL` 唤醒时，快速退出路径没有保存进程终止原因为 `SIGKILL`。内核内部用 137 表示 signal termination 的实现细节泄露为普通 `exit(137)` wait status。

不能对所有 pending `SIGKILL` 在退出门中无条件记录原因：`execve` 去线程化和 `exit_group()` 会用内部 `SIGKILL` 清理 sibling。特别是 execve 成功后进程仍继续运行，若把该内部信号写入共享的 `ProcessMeta.termination_signal`，未来正常退出会被错误报告为 `SIGKILL`。

## 修复

- `add_signal_with_info()` 对带 `SigInfo` 的用户态 `SIGKILL` 在其进入 pending 集时记录 `termination_signal = Some((SIGKILL, false))`。`kill/tkill/tgkill` 均走该路径，且 `SIGKILL` 不可捕获、不可忽略，因此不会重演 `kill12` 中被忽略信号提前污染 wait status 的问题。
- 内部 `send_signal_to_thread(..., SIGKILL)` 不携带 `SigInfo`，不写进程终止状态，保持 execve sibling 清理和正常 `exit_group(code)` 的语义。
- `exit_current_if_group_exited_or_killed()` 提升为 crate 内共享退出门，始终优先使用已有 `group_exit_code`；否则才处理 pending `SIGKILL` 的内部退出码 137。
- `block_on()` 复用该退出门，删除其独立的 `exit_current_and_run_next(137)` 分支，保证 `nanosleep`、future 等待和协作调度不再出现不同的 SIGKILL 处理结果。

## 涉及文件

- `os/src/signal/delivery.rs`
- `os/src/task/mod.rs`
- `os/src/task/future/mod.rs`

## 验证

已执行：

```text
rustfmt --edition 2021 --check os/src/task/mod.rs os/src/task/future/mod.rs os/src/signal/delivery.rs
git diff --check
make TARGET_ARCH=loongarch64
timeout 120s make TARGET_ARCH=loongarch64 run > /tmp/kill07-after-fix.log 2>&1
make TARGET_ARCH=riscv64
```

LoongArch64 的实际 `kill07` 运行结果：

```text
kill07      0  TINFO  :  received expected signal 9
kill07      1  TPASS  :  Did not catch signal as expected

Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
shutdown!
```

输出中的 `FAIL LTP CASE kill07 : 0` 是测试运行器打印的退出码 0 包装行，不是 LTP 失败；应以 Summary 为准。LoongArch64 与 RISC-V release 构建均通过。未运行 RISC-V QEMU 行为回归。

全仓 `cargo fmt --check` 仍报告未触及的 `os/src/arch/loongarch64/qemu/mod.rs` 模块声明排序；本次修改的三个 Rust 文件已单独通过 `rustfmt --check`。
