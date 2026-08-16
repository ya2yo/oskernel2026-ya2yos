# waitpid/waitid 子进程退出事件丢唤醒

## 背景

`waitpid()` 和 `waitid()` 通过父进程 `ProcessMeta` 中的
`child_exit_event` 等待子进程退出、停止或继续。子进程退出时会在获取父进程
`ProcessMeta` 后唤醒该事件。该等待路径同时需要读取每个 child 的状态、写入用户
缓冲区，并在需要时回收 zombie。

## 现象

`log.ans` 最后停在 `RUN LTP CASE setuid04`。该测例只输出一次
`setuid04.c:49: TPASS: open() returned errno EACCES`，没有第二次 `TPASS`、LTP
`Summary`、后续测例或 `shutdown!`。同一测例在 LoongArch64 日志中可输出两个
`TPASS` 并正常结束，说明问题具有 SMP 时序特征。`setuid04` 使用嵌套 `fork`，父进程
随后通过 `waitpid` 回收子进程，容易触发退出与等待并发交错。

## 分析

此前 `waitpid`/`waitid` 的 poll 顺序是：先复制 child 列表并检查 child 状态，确认
没有可返回事件后，才向 `child_exit_event` 注册当前 waker，再返回
`Poll::Pending`。子进程退出路径则在父 `ProcessMeta` 锁内执行事件唤醒。于是可能出现：

1. 父进程检查到 child 尚未退出；
2. 子进程退出，获取父 `ProcessMeta` 并完成 `wake()`；
3. 父进程才注册 waker 并睡眠。

这次唤醒发生在 waker 注册前，之后没有新的事件时父进程会永久阻塞，表现为 QEMU
停在 `setuid04` 的第一个通过结果。

此前为修复跨进程锁嵌套而做的 child 列表快照本身是必要的，但将事件注册推迟到
`Poll::Pending` 分支后引入了上述 check-then-sleep 窗口。

## 根因

`child_exit_event.register(cx.waker())` 晚于 child 状态检查，且注册与子进程退出时的
唤醒没有由同一把父 `ProcessMeta` 锁建立顺序关系，导致 lost wakeup。

## 修复

- `waitpid` poll 开始时先获取父 `ProcessMeta`，在锁内注册 waker，再复制可升级的
  child `Arc` 列表并释放父锁。
- `waitid` 使用相同顺序，覆盖所有 wait-family 阻塞入口。
- child 状态读取、`copy_to_user` 和 zombie 回收继续在父锁释放后进行，避免恢复跨进程
  `ProcessMeta` 锁嵌套或在锁内访问用户内存。
- zombie 回收仍通过 `remove_child_from_parent()` 重新获取父锁并确认归属，防止并发
  waiter 重复回收和重复累计资源使用量。

## 涉及文件

- `os/src/syscall/task/wait.rs`

## 验证

- `make TARGET_ARCH=riscv64`：RISC-V64 release 构建通过；根 Makefile 同次完成
  LoongArch64 release 构建，只有既有 Cargo/vendored `smoltcp` warning。
- RISC-V64 debug 定向 `setuid04`：两个 `TPASS`，`passed 2 failed 0 broken 0 skipped 0`，
  正常输出 `shutdown!`。
- RISC-V64 release 定向 `setuid04`：两个 `TPASS`，`passed 2 failed 0 broken 0 skipped 0`，
  正常输出 `shutdown!`。
- `git diff --check` 通过。

完整 LTP/BuildStorm 尚未重跑；本次已覆盖原始卡死测例及其 debug/release 触发路径。
