# waitpid10: zombie PID 复用与进程组等待

## 背景

LTP `waitpid10` 会 fork 8 个子进程，其中部分子进程还会循环 fork 并 `waitpid(fork_pid)` 回收自己的子进程。测试要求每个 fork 返回的 PID 都能由对应父进程正确 wait 回收。

## 现象

`log.ans` 中失败为：

```text
waitpid_common.h:166: TFAIL: Pid 8 not reaped
waitpid10.c:97: TINFO: reap_children(fork_pid, 0, &fork_pid, 1) failed
waitpid10.c:59: TINFO: reap_children(0, 0, fork_kid_pid, MAXKIDS) failed
```

同时日志里出现：

```text
inserting process 8
expected replacement? 8
sys_waitpid <= pid: 8
sys_waitpid: my children ... []
```

这说明 PID 8 的 zombie 进程还没有被父进程 wait 回收时，新的 fork 已经复用了 PID 8，并覆盖了全局进程表里的旧进程。

## 分析

当前内核用 `TidHandle` 的 Drop 释放 TID/PID。进程退出后 TCB 会先 drop，父进程稍后才通过 `waitpid()` 从 children 和全局进程表移除 zombie。两者之间存在窗口：ID 已经被放回 allocator，但 zombie `Process` 仍然存在。

`waitpid10` 中多个子进程并发 fork/exit，窗口被稳定触发。新进程复用 PID 8 后，`Process::new()` 覆盖 `PID_2_PROCESS_ARC` 中的旧项，旧 zombie 被提前 drop，父进程后续无法再观察并回收它；复用 PID 的直接父进程也可能在全局表覆盖和 weak child 失效后看到空 children，导致 `waitpid(fork_pid)` 返回 `ECHILD`。

日志还显示 `sys_setpgid` 是 stub。虽然这次直接失败由 PID 过早复用触发，但 `waitpid(0)` 和 `waitpid(<-1)` 依赖进程组过滤，继续把 `pid == 0` 当作 any 会在 waitpid 系列测例中留下语义偏差。

## 根因

进程 ID 的生命周期绑定在 TCB 上，而不是 zombie `Process` 被父进程 wait 回收之后；同时内核没有维护 `pgid`，`setpgid/getpgid` 和进程组 wait selector 只是兼容 stub。

## 修复

- `TidHandle::drop()` 不再立即回收共享 TID/PID，避免 zombie wait 前复用 PID 并覆盖全局进程表。
- `ProcessMeta` 增加 `pgid`，init 进程默认 pgid 为自身 pid，fork 子进程继承父进程 pgid。
- 实现基础 `sys_setpgid()` / `sys_getpgid()` / `sys_setsid()`，支持当前进程和直接子进程的 pgid 调整。
- `WaitPid::Pgid` 改为按 `child.pgid()` 过滤；`waitpid(0)` 转为当前进程 pgid，`waitpid(pid < -1)` 转为指定 pgid；`waitid(P_PGID, 0)` 同样按当前 pgid 过滤。

## 涉及文件

- `os/src/task/tid.rs`
- `os/src/task/process/process.rs`
- `os/src/syscall/task/job.rs`
- `os/src/syscall/task/wait.rs`
- `os/src/syscall/mod.rs`

## 验证

已执行：

```text
make
timeout 90s make run
```

结果：

```text
waitpid10.c:62: TPASS: Test PASSED

Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

备注：`make log` 因 `os/target/release/.fingerprint/os-b35de98e01d47e74` 下存在 `nobody:nogroup` 构建产物而无法重写 fingerprint，本次使用已成功构建的 release kernel 运行验证。
