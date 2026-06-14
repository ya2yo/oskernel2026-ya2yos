# waitid 系统调用实现

## 背景

LTP `waitid01` 验证 `waitid(P_ALL, 0, infop, WEXITED)` 能等待子进程正常退出，并正确填写 `siginfo_t` 中的 `si_pid`、`si_status`、`si_signo` 和 `si_code`。

## 现象

内核已在 syscall 表中接入 `WaitId = 95`，但 `sys_waitid()` 只有半截参数校验和未完成的 `match`，无法编译成完整语义，也无法向用户态返回 `siginfo_t`。

## 分析

现有 `sys_waitpid()` 已经实现了子进程筛选、`WNOHANG`、`WNOWAIT`、阻塞等待、信号中断和回收资源统计。`waitid()` 与 `waitpid()` 的核心等待路径相同，差异主要在 selector 类型和返回信息：

- `idtype` 使用 `P_ALL/P_PID/P_PGID/P_PIDFD`，不是 bitflags。
- 成功返回值为 0，结果写入 `siginfo_t`。
- 正常退出时 `si_signo = SIGCHLD`，`si_code = CLD_EXITED`，`si_status` 是未左移的退出码。

原有 `SigInfo` 结构只覆盖到 offset 16 的 pid 字段，缺少 Linux/musl `SIGCHLD` 布局中 offset 24 的 `si_status`。

## 根因

`sys_waitid()` 未完成实现；同时 `SigInfo` 的内核侧布局不足以表达 `waitid()` 必须返回的 child status。

## 修复

- 补齐 `SigInfo` 中 `si_pid/si_uid/si_status` 字段，并新增 `SigInfo::new_child()`。
- `sys_waitid()` 严格校验 options，要求至少包含 `WEXITED/WSTOPPED/WCONTINUED` 之一。
- 显式解析 `P_ALL/P_PID/P_PGID/P_PIDFD`，避免把枚举参数当 bitflags 截断。
- 复用 waitpid 的子进程过滤、阻塞等待、信号中断、`WNOHANG` 和 `WNOWAIT` 回收逻辑。
- `WNOHANG` 无事件时向 `infop` 写入全零 `SigInfo`；有事件时填入 `SIGCHLD/CLD_EXITED/pid/status`。

## 涉及文件

- `os/src/signal/signal.rs`
- `os/src/syscall/task/wait.rs`

## 验证

已执行：

```text
make
timeout 150s make run
```

结果：

```text
waitid01.c:28: TPASS: waitid(P_ALL, 0, infop, WEXITED) passed
waitid01.c:29: TPASS: infop->si_pid == pidchild (4)
waitid01.c:30: TPASS: infop->si_status == 123 (123)
waitid01.c:31: TPASS: infop->si_signo == SIGCHLD (17)
waitid01.c:32: TPASS: infop->si_code == CLD_EXITED (1)

Summary:
passed   5
failed   0
broken   0
skipped  0
warnings 0
```
