# RISC-V 双 hart `test_yield` fork/exit 锁序死锁

## 背景

RISC-V QEMU 开启两个 hart 后，独立进程按 PID 固定到各自的 `home_hart`。默认 CFS 使用 per-Hart runqueue，因此父进程和 `fork()` 出来的子进程可以在不同 hart 上真正并发执行。

`basic_testcode.sh` 的 `test_yield` 会创建三个子进程。每个子进程执行五次 `sched_yield()` 和打印，父进程随后连续三次 `wait(NULL)`。该测试既覆盖 `sched_yield`，也会放大“父进程继续 fork、先创建的子进程快速退出”的并发窗口。

## 现象

无 DEBUG 的 RISC-V release+CFS 日志稳定出现：

```text
Testing yield :
========== START test_yield ==========
  I am child process: 43. iteration 0.
  I am child process: 43. iteration 0.
  I am child process: 43. iteration 0.
  I am child process: 43. iteration 0.
  I am child process: 43. iteration 0.
QEMU: Terminated
```

没有 panic、`TFAIL` 或 `TBROK`。带 DEBUG 日志时测试可能完整通过，说明故障受时序影响。

测试源码和镜像反汇编确认，连续五条 `iteration 0` 本身是正常行为：打印参数是外层 fork 序号 `i`，每个子进程的五轮计数由另一个变量维护。第一名子进程第五次打印后立即调用 `exit(0)`；真正的停滞点在随后发生的退出路径，而不是 `sched_yield()` 没有返回或用户寄存器没有推进。

## 分析

### 测试进程的 hart 分布

RISC-V 当前使用：

```rust
home_hart = (pid - 1) % HART_NUM
```

典型分布为：

```text
parent PID 42 -> hart 1
child  PID 43 -> hart 0
child  PID 44 -> hart 1
child  PID 45 -> hart 0
```

PID 43 开始运行时，PID 44/45 可能尚未创建。它所在的 hart 0 没有其他可运行实体，因此每次 `sched_yield()` 后仍可立即选回自己，并很快完成五轮进入 `exit(0)`。与此同时，父 PID 42 仍可能在 hart 1 创建后续子进程。

### 父进程 clone 路径

修复前 `TaskControlBlock::clone_process()` 先获取父任务的 `TaskControlBlockInner`，随后仍在该锁内读取 `ProcessMeta`、调用 `self.ppid()`，并在普通 fork 分支调用 `Process::new()`。`Process::new()` 会通过 `link_child_to_parent()` 获取父进程的 `ProcessMeta`。

父进程的实际锁链为：

```text
TaskControlBlockInner(parent)
    -> ProcessMeta(parent)
```

### 子进程退出路径

修复前 `exit_current_and_run_next()` 使用：

```rust
for task_weak in &parent.meta_lock().tasks {
    if let Some(task) = task_weak.upgrade() {
        let mut parent_inner = task.inner_lock();
        // 检查并唤醒 vfork parent
    }
}
```

`parent.meta_lock()` 的临时 guard 会保持到整个 `for` 语句结束，因此退出路径的锁链为：

```text
ProcessMeta(parent)
    -> TaskControlBlockInner(parent)
```

### 双 hart AB-BA

两个路径并发后形成完整锁环：

```text
hart 1 / parent:
已持有 TaskControlBlockInner(parent)
    -> 等待 ProcessMeta(parent)

hart 0 / child:
已持有 ProcessMeta(parent)
    -> 等待 TaskControlBlockInner(parent)
```

DEBUG 日志会显著拖慢子进程的 syscall、缺页和打印路径，使父进程更可能先完成三次 fork，从而躲开死锁窗口。这也是 DEBUG 构建不能单独证明修复有效的原因。

## 根因

根因是 clone 与 exit 对同一父进程对象采用了相反的锁顺序，违反 `os/src/task/mod.rs` 规定的：

```text
ProcessMeta -> TaskControlBlockInner
```

CFS 和 per-Hart 调度改变了父子进程的相对速度，使这个既有锁序错误更容易复现；`sys_sched_yield()`、trap PC 推进和整数寄存器保存并不是本次卡死根因。

## 修复

- `clone_process()` 先在独立短临界区快照父进程的 `parent_pid`、`pgid`、`sid` 和 `comm`，释放 `ProcessMeta` 后再读取父任务状态。
- 持有父 `TaskControlBlockInner` 时不再调用会获取 `ProcessMeta` 的 `self.ppid()`，改用已快照的 `parent_pid`。
- 将会登记父子关系、获取父 `ProcessMeta` 的 `Process::new()` 移到父任务 inner 锁释放之后。
- 子进程退出时先复制父进程的 weak task 列表并释放 `ProcessMeta`，再逐个获取父任务 inner 锁。
- `exit_current_group_and_run_next()` 同样先快照线程列表，不再无必要地持有当前任务 inner 后获取进程元数据。

修复没有修改 `sched_yield` ABI、CFS 选取策略或 testcase。

## 涉及文件

- `os/src/task/task/task.rs`
- `os/src/task/mod.rs`

## 验证

- 修复前无 DEBUG 的 `timeout 70s make run > log.ans 2>&1` 可复现：musl 偶尔完整结束，glibc 在首名 child 的五条输出后超时。
- `cargo fmt -- --check`：通过。
- `git diff --check`：通过。
- `make SCHEDULER=cfs`：RISC-V64 与 LoongArch64 release 构建均通过；只有既有 Cargo 配置弃用提示和 vendored `smoltcp` warning。
- 修复后重复执行 RISC-V release+CFS basic 回归，多个有效样本的 musl/glibc 均各输出 15 条 child 行、出现 `END test_yield` 和 group END，最后正常 `shutdown!`。最终结果保存在仓库根目录 `log.ans`。
- 独立复核的 RISC-V release 运行同样返回 0，并完整通过两组 `test_yield`。
- LoongArch64 单核运行完成 musl/glibc 两组 basic 和 `shutdown!`，验证共享 clone/exit 代码未发生运行时回归。

另有一个 RISC-V 启动样本在进入用户态前停于 `task::add_initproc`，未进入本测例，也未计入 `test_yield` 回归；清理临时 `disk.img` 后后续样本完整通过。

## 剩余风险

`exec()` 的 vfork parent 唤醒路径仍值得单独审计：它存在持有当前 task inner 时读取进程/父进程元数据的旧锁域。`clone_process()` 中 `CLONE_PARENT_SETTID` 用户内存写入和部分资源复制的 inner 锁临界区也可继续缩短。它们没有参与本次普通 fork/exit 复现，因此未扩大本补丁范围。
