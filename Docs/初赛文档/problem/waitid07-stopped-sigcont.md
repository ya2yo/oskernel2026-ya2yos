# waitid07: WSTOPPED 与 SIGCONT 后 checkpoint 超时

## 背景

LTP `waitid07` 验证 `waitid(P_PID, child, infop, WSTOPPED | WNOWAIT)` 能观察到子进程被 `SIGSTOP` 停止，并在父进程 `SIGCONT` 后继续执行 checkpoint 同步。

测试主体流程可以简化为：

```c
pid_child = SAFE_FORK();
if (!pid_child) {
	SAFE_KILL(getpid(), SIGSTOP);
	TST_CHECKPOINT_WAIT(0);
	return;
}

TST_EXP_PASS(waitid(P_PID, pid_child, infop, WSTOPPED | WNOWAIT));
TST_EXP_EQ_LI(infop->si_pid, pid_child);
TST_EXP_EQ_LI(infop->si_status, SIGSTOP);
TST_EXP_EQ_LI(infop->si_signo, SIGCHLD);
TST_EXP_EQ_LI(infop->si_code, CLD_STOPPED);

SAFE_KILL(pid_child, SIGCONT);
TST_CHECKPOINT_WAKE(0);
```

因此这个测例同时覆盖三类语义：

- 默认停止信号：`SIGSTOP` 不能被忽略，必须让子进程进入 stopped 状态。
- `waitid(WSTOPPED)`：父进程不能只等待 exit，也要能观察 stopped child event。
- `SIGCONT` 后继续运行：子进程恢复后应继续执行 checkpoint wait，并被父进程 checkpoint wake 唤醒。

## 现象

`log.ans` 中 `waitid07` 最初表现为 checkpoint 超时：

```text
waitid07.c:26: TBROK: tst_checkpoint_wait(0, 10000) failed: ETIMEDOUT (110)
```

修复停止事件前，根本问题是子进程发给自己的 `SIGSTOP` 没有真正停止，父进程也没有可等待的 `WSTOPPED` 事件。补齐 stopped 状态和 `waitid(WSTOPPED)` 后，测试推进到前 5 项 `TPASS`，但仍在 checkpoint 阶段超时：

```text
waitid07.c:31: TPASS: waitid(P_PID, pid_child, infop, WSTOPPED | WNOWAIT) passed
waitid07.c:33: TPASS: infop->si_pid == pid_child (4)
waitid07.c:34: TPASS: infop->si_status == SIGSTOP (19)
waitid07.c:35: TPASS: infop->si_signo == SIGCHLD (17)
waitid07.c:36: TPASS: infop->si_code == CLD_STOPPED (5)
waitid07.c:26: TBROK: tst_checkpoint_wait(0, 10000) failed: ETIMEDOUT (110)
```

## 分析

### 第一阶段：停止事件缺失

原信号处理逻辑只区分“自定义 handler”和“默认忽略/终止”，没有根据 `SigSet::default_op()` 处理 `SigOp::Stop`。结果是 `SIGSTOP` 类默认停止信号没有让任务进入 stopped 状态，也没有通知父进程。`sys_waitid()` 也只扫描退出 child，不能返回 `CLD_STOPPED`。

这部分需要补齐两块状态：

- task 级状态：新增 `TaskStatus::Stopped`，调度器不能把 stopped task 自动重新放回 ready queue。
- process 级事件：新增 `ProcessMeta::stopped_signal`，用于 `waitid(WSTOPPED)` 返回 `si_status = SIGSTOP`，并在非 `WNOWAIT` 时清除该 stopped event。

`WNOWAIT` 的含义也很关键：父进程本次只是观察 stopped event，不能消费它对应的子进程对象，更不能回收 child。对 stopped event 来说，`WNOWAIT` 表示 `stopped_signal` 保留；非 `WNOWAIT` 才清除这个 event。

### 第二阶段：checkpoint futex 被 SIGCONT 中断

补齐 stopped event 后，`waitid07` 的 siginfo 检查全部通过，但仍然在 `TST_CHECKPOINT_WAIT(0)` 超时。这里容易误判为父进程 `SIGCONT` 后立刻 `TST_CHECKPOINT_WAKE(0)`，单核非抢占调度下父进程抢跑，导致 wake 早于子进程 wait。

为了确认真实原因，临时加 futex 日志观察 checkpoint 同步路径，看到如下顺序：

```text
pid=4 cmd=Wait uaddr=0x2a2344601c pa=0x8362601c key=0x8362601c val=0
pid=3 cmd=Wake uaddr=0x2a2344601c pa=0x8362601c key=0x8362601c val=2147483647
pid=4 cmd=Wait uaddr=0x2a2344601c pa=0x8362601c key=0x8362601c val=0
```

这说明：

- 父子进程看到的是同一个 futex 用户地址和同一个物理地址，futex key 一致。
- 父进程确实执行了 `FUTEX_WAKE`。
- 子进程第一次 wait 已被唤醒，但随后又进入第二次 wait。

LTP checkpoint 的 wait 实现是：

```c
do {
	ret = syscall(SYS_futex, &tst_futexes[id], FUTEX_WAIT,
		      tst_futexes[id], &timeout);
} while (ret == -1 && errno == EINTR);
```

也就是说，子进程只有在 futex syscall 返回 `EINTR` 时才会再次等待。父进程只 wake 一次，子进程二次 wait 后自然会超时。由此可以把问题从“futex key 不一致 / wake 丢失”缩小到“子进程被某个 pending signal 中断”。

### 为什么是 pending SIGCONT

子进程处理 `SIGSTOP` 的路径如下：

1. 子进程执行 `kill(getpid(), SIGSTOP)`，信号进入 pending。
2. trap 返回用户态前调用 `handle_signal(SIGSTOP)`。
3. 默认 `SIGSTOP` 分支记录 `stopped_signal`，唤醒父进程，并调用 `stop_current_and_run_next()` 切出调度。
4. 父进程 `waitid(WSTOPPED | WNOWAIT)` 成功，随后 `kill(child, SIGCONT)`。
5. `SIGCONT` 投递到 stopped child 后，把 child 放回 ready queue。
6. child 重新被调度，`stop_current_and_run_next()` 返回，控制流继续回到原来的 `handle_signal(SIGSTOP)` 之后。

问题出在第 6 步之后：原 `trap_return()` 每次只处理一个 pending signal。它刚处理完 `SIGSTOP`，不会继续检查 pending `SIGCONT`，于是直接回到用户态执行 `TST_CHECKPOINT_WAIT(0)`。此时 `SIGCONT` 仍在 pending 集合里。futex wait 被父进程 wake 后返回内核，返回路径看到未处理的 pending `SIGCONT`，把本次阻塞 syscall 当成被信号中断，返回 `EINTR`。LTP 看到 `EINTR` 后再次 futex wait，但父进程已经不会再 wake，最终超时。

因此，调度让步只能缓解“父进程抢跑”的可能性，不能解决这个根因。真正需要修的是 signal return 路径：默认信号不需要构造用户态 signal frame，可以在同一次 `trap_return()` 中连续消费；只有遇到用户自定义 handler 时才必须停止循环并立即返回用户态执行 handler，避免连续构造多个 signal frame 覆盖用户栈。

### MAP_SHARED 与 RISC-V Dirty 位

排查 checkpoint 时同时检查了共享 mmap，因为 LTP checkpoint 的 futex 存放在共享映射中。这里发现两类独立但相关的问题。

第一，`MapArea::new_mmap()` 对 `MAP_SHARED` 的 `GROUP_SHARE` 条件写反。旧逻辑让 `MAP_SHARED` 的 `groupid = 0`，而把 `MAP_PRIVATE` 加入 `GROUP_SHARE`。这会导致 fork 后真正的共享映射无法通过 `groupid + vpn` 找到同一共享帧，破坏 lazy mmap 下父子共享页的一致性。正确语义是：

- `MAP_SHARED` 分配非 0 `groupid`，fork 子进程继承同一个 groupid，并增加 group 引用计数。
- `MAP_PRIVATE` 不进入 `GROUP_SHARE`，按私有/COW 语义处理。

第二，RISC-V PTE 的 `DIRTY` 位需要按映射语义设置。对 `MAP_SHARED | PROT_WRITE` 页面，即使当前是 read fault 建立映射，只要 VMA 本身允许写，PTE 也应保持 writable + dirty。原因是 RISC-V 的 store 检查不仅看 `W`，还会看 `D`。若 PTE 是 `W=1, D=0`，后续 store 可能触发 StorePageFault / PageModifyFault，让内核进入写 fault 路径。

不能在异常里对所有 `D=0` 页面无条件置 `D`，因为本内核也用 `W/D/COW` 组合表达写保护和 COW：

- fork 后的私有页第一次写必须复制，不能直接在原物理页上写。
- `MAP_PRIVATE` 文件映射第一次写必须生成私有页，不能污染共享文件页。
- 只读映射或权限错误不能因为 `D=0` 就放行写入。

因此这次只对 `MAP_SHARED` 且 VMA 可写的 mmap PTE 插入 `DIRTY`。`MAP_PRIVATE` 仍清 `WRITEABLE/DIRTY` 并设置 `COW`，等待写 fault 时走 COW。

## 根因

直接根因有两个：

1. 默认 `SIGSTOP` 没有让任务进入 stopped 状态并通知父进程，`waitid(WSTOPPED)` 无事件可返回。
2. `trap_return()` 只处理一个 pending signal，`SIGSTOP` 恢复后遗留默认 `SIGCONT`，导致 checkpoint futex wait 被错误中断为 `EINTR`，LTP 二次 wait 后无人再 wake。

关联修复项：

- `MAP_SHARED` groupid 条件写反，会破坏 fork 后共享 mmap lazy fault 的共享帧复用。
- RISC-V shared mmap 可写页未区分 `MAP_SHARED` / `MAP_PRIVATE`，`D=0` 可导致后续 store 进入错误的写 fault / COW 路径。

## 修复

- 增加 `TaskStatus::Stopped` 和 `ProcessMeta::stopped_signal`，默认停止信号记录停止原因、唤醒父进程 wait 事件，并按 `SA_NOCLDSTOP` 决定是否发送 `SIGCHLD`。
- `sys_waitid()` 支持 `WSTOPPED`，填充 `SIGCHLD / CLD_STOPPED / si_status = stop_signal`，并遵守 `WNOWAIT`。
- `SIGCONT` / `SIGKILL` 投递到停止任务时将任务放回 ready queue；`kill(SIGCONT)` 在确实恢复停止任务后让出一次调度。
- `trap_return()` 改为连续消费默认信号；遇到用户自定义 handler 时停止循环并返回用户态运行 handler，避免覆盖信号栈帧。
- 修正 `MAP_SHARED` 的 `GROUP_SHARE` 分组条件，fork 继承共享 mmap 时增加 group 引用计数。
- RISC-V mmap fault 对 `MAP_SHARED | PROT_WRITE` 设置 writable + dirty，表示后续 store 可以直接写同一共享物理页；`MAP_PRIVATE` 保持 COW。

修复后的关键控制流是：

```text
child kill(SIGSTOP)
  -> handle_signal(SIGSTOP)
  -> child TaskStatus::Stopped, parent child_exit_event wake
  -> parent waitid(WSTOPPED | WNOWAIT) returns CLD_STOPPED
  -> parent kill(child, SIGCONT)
  -> child Stopped -> Ready
  -> child resumes trap_return
  -> trap_return consumes pending default SIGCONT
  -> child enters TST_CHECKPOINT_WAIT
  -> parent TST_CHECKPOINT_WAKE wakes child
  -> test exits normally
```

## 涉及文件

- `os/src/signal/mod.rs`
  - 默认 `SIGSTOP` / `SIGCONT` 语义。
  - `add_signal()` 返回是否恢复 stopped task，便于 `kill(SIGCONT)` 判断是否调度让步。
- `os/src/syscall/signal.rs`
  - `sys_kill()` 保持用户可见返回值为 0；当 `SIGCONT` 确实恢复 stopped task 后让出一次调度。
- `os/src/syscall/task/wait.rs`
  - `sys_waitid()` 支持 `WSTOPPED`、`CLD_STOPPED`、`WNOWAIT`。
- `os/src/task/mod.rs`
  - 新增 `stop_current_and_run_next()`。
- `os/src/task/processor.rs`
  - 调度器不自动重新入队 stopped task。
- `os/src/task/process/process.rs`
  - `ProcessMeta::stopped_signal` 保存 stopped event。
- `os/src/task/task/task.rs`
  - 新增 `TaskStatus::Stopped`。
- `os/src/trap/mod.rs`
  - `trap_return()` 连续处理默认 pending signal。
- `os/src/mm/map_area.rs`
  - `MAP_SHARED` 分配共享 groupid，`MAP_PRIVATE` 不加入 `GROUP_SHARE`。
- `os/src/mm/memory_set/fork_clone.rs`
  - fork 继承 `MAP_SHARED` groupid 并增加 group 引用计数。
- `os/src/mm/page_fault_handler.rs`
  - mmap fault 统一把 `mmap_flags` 传给架构页表处理函数。
- `os/src/arch/riscv64/qemu/page_table.rs`
  - `MAP_SHARED | PROT_WRITE` 保留 writable + dirty；`MAP_PRIVATE` 走 COW。

## 验证

已执行：

```text
make
timeout 150s make run
```

结果：

```text
waitid07.c:31: TPASS: waitid(P_PID, pid_child, infop, WSTOPPED | WNOWAIT) passed
waitid07.c:33: TPASS: infop->si_pid == pid_child (4)
waitid07.c:34: TPASS: infop->si_status == SIGSTOP (19)
waitid07.c:35: TPASS: infop->si_signo == SIGCHLD (17)
waitid07.c:36: TPASS: infop->si_code == CLD_STOPPED (5)

Summary:
passed   5
failed   0
broken   0
skipped  0
warnings 0
```

修复过程中的临时 futex 日志也验证过父子 futex key 一致，最终代码中已删除这些临时日志。
