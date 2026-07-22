# LTP fcntl14 rt_sigsuspend 临时信号掩码恢复时机修复

## 背景

LTP `fcntl14` 除了验证 POSIX advisory record lock 的区间语义，还用信号同步父、子和孙进程的
加锁顺序。父进程先 `sighold(SIGUSR1)`，在持有 record lock 后通过 `sigpause(SIGUSR1)` 临时
解除对 `SIGUSR1` 的屏蔽并等待子进程通知。子进程发送该信号后，父进程的 `catch1()` 应使
`got1` 递增，父进程随后才释放锁并进入下一组断言。

`sigpause()` 经 libc 最终使用 `rt_sigsuspend()`。因此该用例同时覆盖 record lock 和临时
signal mask 的交接语义。

## 现象

原始 LoongArch64 `log.ans` 中，musl `fcntl14` 为 `passed 11 failed 129 broken 0`，glibc 为
`passed 67 failed 37 broken 0`。典型输出为：

```text
Pause terminated without signal SIGUSR1 from child
GETLK: pid = 0, should be parent's id of 2
SETLK: rc = 0, errno = 4, -1/EAGAIN or EACCES was expected
```

`F_GETLK` 返回 `F_UNLCK/l_pid = 0`，并且本应冲突的子进程 `F_SETLK` 意外成功。日志中的
`fcntl14 : 256` 是测试进程 `exit(1) << 8` 的 wait status，而不是 256 个失败断言。

## 分析

先审计 record lock 路径。子进程在最早的冲突检查中能够观察到父锁，现有实现也覆盖了本用例实际
执行的 `SEEK_CUR`、负 `l_start`、进程 owner、`close()` 和退出释放路径；锁表不是首次失败原因。
父进程提前释放锁后，后续 `F_GETLK/F_SETLK` 才出现连锁失败。

测试源码的关键同步点如下：

- `fcntl14.c:815` 调用 `sighold(SIGUSR1)`；
- `fcntl14.c:833` 调用 `sigpause(SIGUSR1)`；
- `fcntl14.c:837` 检查 `got1 == 1`；
- `fcntl14.c:896` 至 `:909` 的 `catch1()` 增加 `got1`。

临时诊断日志显示 `sys_rt_sigsuspend()` 已观察到 pending `SIGUSR1(10)` 并返回 `EINTR`，但随后
没有 `handle_signal signo=10`，用户态直接继续执行 `Fcntl`。这排除了 `SIGCHLD` 抢先唤醒的
假设。

旧路径在装入 `sigpause()` 的临时 mask 后发现可交付信号，却在返回 `EINTR` 前立即恢复了
`old_mask`。`old_mask` 正是 `sighold(SIGUSR1)` 设置的屏蔽集。之后 `trap_return()` 用
`sig_pending - sig_mask` 选择待交付信号时，刚到达的 `SIGUSR1` 已再次被屏蔽，因而无法建立
signal frame、执行 `catch1()` 或清除 pending。下一次 `sigpause()` 再临时解除屏蔽时会错误消费
该遗留信号，破坏下一轮锁同步。

## 根因

`rt_sigsuspend` 的两个 mask 生命周期被混为一体：

- 临时 mask 必须保留到 trap return，才能选择并投递唤醒该 syscall 的信号；
- 调用前 mask 必须保存到用户 signal frame，待 handler 的 `rt_sigreturn()` 后恢复。

旧实现用同一 `sig_mask` 表示两者，并在 signal frame 建立前恢复了调用前值，导致真实的
`SIGUSR1` 变回不可交付状态。

## 修复

- 在 `TaskControlBlockInner` 增加 `sigsuspend_restore_mask`，并在新建 task、clone child 和
  exec 重置时初始化/清除。
- `sys_rt_sigsuspend()` 命中不可忽略的 pending signal 后保留临时 mask，将调用前 mask 记录在
  `sigsuspend_restore_mask`，随后返回 `EINTR`。默认忽略或显式 `SIG_IGN` 的信号继续被消费后
  等待，不错误打断 `sigsuspend`。
- `setup_frame()` 分离 `active_sig_mask` 与 `restore_sig_mask`：前者叠加当前信号和
  `sa_mask` 作为 handler 运行时 mask；后者写入传统 frame 的 `SigSet` 或 `SA_SIGINFO` frame
  的 `UserContext.sigmask`。状态通过 `take()` 只由第一个真实 handler frame 消费，嵌套
  handler 不会重复使用外层的旧 mask。
- `trap_return()` 在所有 pending signal 都被默认/忽略路径消费、即将无 handler 返回用户态时，
  兜底恢复并清除保存的旧 mask，避免状态遗留给无关的后续 handler。

## 涉及文件

- `os/src/task/task/task.rs`
- `os/src/syscall/signal.rs`
- `os/src/signal/frame.rs`
- `os/src/trap/mod.rs`

## 验证

执行：

```text
git diff --check
make build-arch TARGET_ARCH=loongarch64
timeout 180s make run TARGET_ARCH=loongarch64
make build-arch TARGET_ARCH=riscv64
timeout 180s make run TARGET_ARCH=riscv64
```

结果：

- `git diff --check` 通过。
- LoongArch64 release 构建通过；仅有既有 `smoltcp` 未使用项 warning。
- LoongArch64 定向 QEMU 使用当前工作区的 `fcntl14` 入口，musl/glibc 均为
  `passed 96 failed 0 broken 0`，共 192 个 `TPASS`，无 `TFAIL`、`TBROK`、panic 或
  `Pause terminated without signal SIGUSR1`。
- RISC-V release 构建通过。
- RISC-V QEMU 已实际启动，但当前 final-2026 镜像不包含 `fcntl14` 可执行文件，musl/glibc 均在
  `execve fail!` 后得到 `passed 0 failed 1 broken 0`；这是测试镜像覆盖限制，不能作为 RISC-V
  运行时回归结果。

测试源码与镜像均保持只读，临时诊断日志只写入 `/tmp/`。
