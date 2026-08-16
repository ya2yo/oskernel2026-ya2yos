# LTP setuid04 open 权限检查任务锁自锁

## 背景

LTP `setuid04` 在调用 `setuid(nobody)` 后，由父子进程并发以写方式打开同一个无写权限
文件，预期两个进程均得到 `EACCES`。该路径会经过 `open_inner()` 的现有文件权限检查和
lwext4 inode 元数据读取。

## 现象

新的 `log.ans` 仍停在：

```text
RUN LTP CASE setuid04
setuid04.c:49: TPASS: open() returned errno EACCES
```

之后没有第二个 `TPASS`、LTP `Summary`、后续测例或 `shutdown!`。此前
`waitpid`/`waitid` 的 child-exit waker 丢失修复已包含在该内核中，因而不是这次
仍然卡住的原因。

## 分析

使用 RISC-V QEMU GDB stub 中断卡死现场，两个运行 hart 都在
`RemoteTlbMutex<TaskControlBlockInner>::lock()` 的自旋路径。一个 hart 的完整等待链为：

```text
open_inner -> inode.fstat -> lwext4 TaskMutex::lock -> block_on
    -> exit_current_if_group_exited_or_killed -> TaskControlBlock::inner_lock
```

另一个 hart 同时在定时器维护中扫描任务并申请同一个 `TaskControlBlockInner`。这说明
remote-TLB 自旋只是被阻塞任务锁的表象，不是根因。

原 `open_inner()` 在权限检查开始时取得当前任务的 `TaskControlBlockInner`，随后仍持有
该 guard 调用 `inode.fstat()` 和 `inode.fmode()`。lwext4 元数据操作在 `TaskMutex` 竞争时
可能进入 `block_on()`；而 `block_on()` 每轮都会运行统一退出检查，该检查再次获取当前任务的
`TaskControlBlockInner`。同一任务于是对非重入锁自锁，所有相关 hart 最终忙等。

## 根因

现有文件写权限检查让 `TaskControlBlockInner` guard 跨越了可能阻塞的 VFS/lwext4 元数据操作，
违反任务锁不得跨 `block_on()`、文件系统或其他可阻塞路径的锁边界。

## 修复

`open_inner()` 现在只短暂获取任务 inner lock，快照 `user_id`、`effective_uid` 和
`effective_gid` 后立即释放。后续 inode metadata 查询及所有 owner/group/other 写权限判断
只使用这三个凭据快照，保持同一次 `open()` 的权限判定值，同时保证可阻塞路径中不再持有当前
任务锁。

## 涉及文件

- `os/src/fs/kernel_fs_ops/open.rs`

## 验证

- 使用无临时插桩的 RISC-V64 release 内核，临时将入口改为连续运行 64 次 `setuid04`；64 次均有
  两个 `TPASS`，并均为 `passed 2 failed 0 broken 0 skipped 0 warnings 0`，最终输出
  `shutdown!`。压力入口已恢复，未保留在工作区。
- 恢复正常入口后，`make TARGET_ARCH=riscv64 build-arch`：通过。
- `make TARGET_ARCH=loongarch64 build-arch`：通过。

完整 LTP/BuildStorm 尚未以此修复重跑；本次运行验证仅覆盖 RISC-V64 的目标死锁路径，
LoongArch64 完成编译回归验证。
