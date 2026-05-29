# wait4 阻塞未处理信号导致死循环

### 背景

运行 libctest、lmbench 等测例时，用户态进程在 `wait4` 上长时间不返回，表现为测例卡死。

### 现象

通过反汇编用户态二进制，定位到测试进程在 `wait4` 系统调用处反复陷入内核又立即返回，形成忙等式死循环。

### 分析

`sys_waitpid` 在子进程尚未退出时会注册 `child_exit_event` 并返回 `Poll::Pending`，任务进入阻塞。但进入睡眠前**未检查当前线程是否有待处理信号**。

当子进程退出时父进程可能已收到 `SIGCHLD` 等信号；若信号在 wait 阻塞期间保持 pending，按 Linux 语义应在可被中断的阻塞点上返回 `EINTR`（无 `SA_RESTART` 时），或消费可忽略信号后继续等待。原实现直接睡眠，导致用户态无法按预期处理信号。

### 修复

在 `os/src/syscall/task/wait.rs` 的 `Poll::Pending` 分支前增加信号检查：

- `SIGCHLD`、`SIG_IGN`、默认 Ignore 的信号：清除 pending 后继续 wait
- 自定义 handler 且未设 `SA_RESTART`：返回 `EINTR`
- 其他情况保持 pending 并注册 waker

同时使用 `block_on(interruptible(...))` 包装，使 wait 可被信号打断。

### 涉及文件

- `os/src/syscall/task/wait.rs`
- `os/src/syscall/mod.rs`（`wait4` 路由）

### 验证

libctest、lmbench 测例通过。

---
