# LTP getpriority02 选择器语义修复

## 背景

LTP `getpriority02` 验证 `getpriority(2)` 的非法选择器和不存在目标错误码。测试用例依次要求：

- `which = -1, who = 0` 返回 `EINVAL`；
- `PRIO_PROCESS`、`PRIO_PGRP`、`PRIO_USER` 配合 `who = -1` 返回 `ESRCH`。

## 现象

原 `log.ans` 中，`getpriority02` 的四项断言仅 `PRIO_PROCESS, -1` 通过；其余三项为：

```text
TFAIL: getpriority(-1, 0) succeeds unexpectedly, returned 0
TFAIL: getpriority(1, -1) succeeds unexpectedly, returned 0
TFAIL: getpriority(2, -1) succeeds unexpectedly, returned 0
```

## 分析

`getpriority02.c` 将 `-1` 传给内核 ABI；在内核的无符号 `usize` 参数中，该值为最大无符号整数，不会匹配任何 PID、进程组或 UID。Linux 因而应报告不存在目标的 `ESRCH`。

`getpriority01` 同时覆盖 `PRIO_PROCESS`、`PRIO_PGRP`、`PRIO_USER` 与 `who = 0` 的成功路径，因此只把非 `PRIO_PROCESS` 直接改为错误不足以保持完整的已覆盖 ABI。

## 根因

`sys_getpriority()` 仅实现了 `PRIO_PROCESS`。对其他 `which` 值，包括非法值和合法的 `PRIO_PGRP`/`PRIO_USER`，原实现都直接返回调用线程的优先级作为“退化行为”。这违反 Linux ABI，使无效选择器未返回 `EINVAL`，且不存在的组或用户被伪装成当前进程。

## 修复

修改 `os/src/syscall/task/resource.rs`：

- 用 `match` 显式接受三个 Linux `PRIO_*` 选择器，其余值返回 `EINVAL`；
- `PRIO_PROCESS` 按 PID 查询，进程已没有可用线程时也返回 `ESRCH`；
- `PRIO_PGRP` 按目标 PGID 遍历任务表；`who = 0` 取调用进程的 PGID；
- `PRIO_USER` 按真实 UID 遍历任务表；`who = 0` 取调用线程的真实 UID；
- 对组和用户选择器，返回全部匹配任务中数值最小的 nice 值对应的内核优先级；没有匹配项返回 `ESRCH`。

任务表遍历不会持有全局表锁进入 `ProcessMeta` 或 `TaskControlBlockInner`。`PRIO_PROCESS` 路径也只在 `ProcessMeta` 锁内复制任务引用，随后释放该锁再读取 nice，符合任务模块的锁顺序约束。

## 涉及文件

- `os/src/syscall/task/resource.rs`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

- `make TARGET_ARCH=riscv64`：通过；根 Makefile 同次执行的 LoongArch64 release 构建也通过。
- `git diff --check`：通过。
- 执行 `timeout 600s make TARGET_ARCH=riscv64 run`。QEMU 成功启动，但维护者当前 `initproc` 会先执行 busybox、lua、iperf 等完整序列，运行在前序 `iperf REVERSE_TCP` 阶段停止推进，未进入 LTP；已终止该专属 QEMU 进程组。故本次没有将 `getpriority02` 记为运行通过。
