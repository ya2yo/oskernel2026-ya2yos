# LTP prctl04 seccomp strict/filter 语义

## 背景

`prctl(2)` 的 `PR_SET_SECCOMP` 可为调用线程启用 strict 或 filter 模式，
`PR_GET_SECCOMP` 返回当前模式。strict 仅允许 `read(2)`、`write(2)`、原始
`exit(2)` 和 `rt_sigreturn(2)`；其他 syscall 应终止线程。filter 模式则由用户提供
的 classic BPF 程序决定动作，fork/clone 的子线程或子进程需继承调用者的限制。

LTP `prctl04` 使用一个十条指令的 filter，只允许 `waitid`、`rt_sigprocmask`、
`close`、`exit`、`wait4`、`write` 和 `clone`，其他调用返回 `SECCOMP_RET_KILL`。
该用例分别检查 strict 的 `SIGKILL`、filter 的 `SIGSYS`、原始 `exit` 与 libc
`exit_group` 的区别，以及 filter 的 fork 继承。

## 现象

根目录旧 `log.ans` 的 musl 与 glibc 均有 2 个 `TPASS` 和 7 个 `TFAIL`：

- strict 安装返回成功后，`PR_GET_SECCOMP` 和 `close(2)` 仍可继续执行；
- filter 安装固定返回 `EACCES`，四个 filter 子例均无法开始；
- 因 filter 未生效，libc `exit()` 使用的 `exit_group(2)` 也未被拒绝。

`PR_GET_SECCOMP => Ok(0)` 只能描述 disabled 状态，不能让已经安装的策略影响后续
syscall；仅在 `sys_prctl()` 内判断也无法拦截 `close`、`exit_group` 等其他入口。

## 根因

原 `PR_SET_SECCOMP(SECCOMP_MODE_STRICT)` 只返回 `Ok(0)`，没有在线程控制块保存状态，
统一 syscall 分发也没有策略检查。`SECCOMP_MODE_FILTER` 则被硬编码为 `EACCES`，即使
当前 LTP 进程具有 `CAP_SYS_ADMIN` 也无法安装 filter。

首次实现后，运行日志还显示 LTP 的 variadic `prctl()` 会保留未使用参数寄存器中的值。若
strict/filter 分支错误要求 `arg3..arg5 == 0`，合法调用会收到 `EINVAL`。Linux 对这些未使用
参数不作该校验，因此最终实现只读取 mode 所需的参数。

## 修复

- 新增线程级 `SeccompState::{Disabled, Strict, Filter}` 和 `no_new_privs`；任务创建时为
  disabled，fork/clone 复制调用线程的状态。
- `PR_GET_SECCOMP` 返回实际 mode；strict 只允许从 disabled 进入；filter 要求
  `CAP_SYS_ADMIN` 或 `PR_SET_NO_NEW_PRIVS`，空 `sock_fprog` 指针返回 `EFAULT`。
- filter 的 `sock_fprog` 和 `sock_filter` 均用 `copy_from_user*()` 复制到内核内存，长度上限为
  4096，整个用户内存访问过程不持有 task 锁。
- 实现并验证 `LD W ABS seccomp_data.nr`、`JEQ K`、`RET K` 三种 classic BPF 指令；非法
  指令或越界跳转在安装时返回 `EINVAL`，不会被默认为允许。
- 在统一 `syscall()` 分发入口、执行具体 handler 前应用策略：strict 拒绝路径投递 `SIGKILL`，
  filter 的非 `SECCOMP_RET_ALLOW` 路径投递 `SIGSYS`。既有 signal/trap return 路径负责写入
  终止状态，因此父进程的 `waitpid()` 可观察到 LTP 期望的信号。

## 验证

执行：

```bash
make build-arch TARGET_ARCH=riscv64
make log
timeout 120s make run > /tmp/prctl04-riscv-after.log 2>&1

make build-arch TARGET_ARCH=loongarch64
make log TARGET_ARCH=loongarch64
timeout 120s make run TARGET_ARCH=loongarch64 > /tmp/prctl04-loongarch64-after.log 2>&1
```

两架构均构建成功，QEMU 均正常 `shutdown!`。两套镜像中的 musl 与 glibc `prctl04` 都输出：

```text
passed   9
failed   0
broken   0
skipped  0
warnings 0
```

日志还直接确认了 strict 拒绝路径为 `SIGKILL`，filter 拒绝路径为 `SIGSYS`，以及 filter
继承子进程因 `SIGSYS` 结束。

## 未实现边界

- 当前仅执行本用例覆盖的 classic BPF 指令子集；其他 load、ALU、jump、memory 指令会在安装时
  返回 `EINVAL`。
- 未实现 seccomp filter 叠加、`SECCOMP_FILTER_FLAG_TSYNC`、`SECCOMP_RET_ERRNO/TRAP/TRACE/LOG`
  等动作、listener 和独立 `seccomp(2)` syscall。
- filter 拒绝路径依赖当前 signal 子系统的默认 `SIGSYS` 终止行为；本轮没有覆盖用户自定义
  `SIGSYS` handler 或完整的 seccomp `siginfo` ABI。

## 涉及文件

| 路径 | 修改内容 |
| --- | --- |
| `os/src/task/seccomp.rs` | 线程级状态、classic BPF 子集验证与执行。 |
| `os/src/task/task/task.rs` | task 初始化、clone 继承和策略查询。 |
| `os/src/syscall/sys/prctl.rs` | `PR_SET/GET_SECCOMP`、`no_new_privs`、capability 与用户内存校验。 |
| `os/src/syscall/mod.rs` | syscall handler 前的 seccomp 强制执行。 |
