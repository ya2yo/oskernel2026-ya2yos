# LTP kill10 ppoll/ITIMER 阻塞唤醒卡死

## 背景

LTP `kill10` 创建 master、manager 和 child 进程组，以 `SIGUSR1`/`SIGUSR2`
进行高频同步。为避免在信号先到、随后才调用 `pause()` 时永久等待，测试在每次
`pause()` 前调用 `alarm(1)`。

测试镜像中的 musl 将 `pause()` 实现为：

```text
ppoll(NULL, 0, NULL, NULL)
```

因此该路径必须可靠地等待普通 signal，并使 `ITIMER_REAL` 到期后的 `SIGALRM`
唤醒等待者。

## 现象

`make log` 版本能完成，而 RISC-V release+CFS 版本在 `kill08` 通过后停在：

```text
RUN LTP CASE kill10
["kill10\\0"]
```

120 秒内没有 LTP summary。GDB 显示一个 hart 反复从
`sys_ppoll()` 的 signal 检查和 `suspend_current_and_run_next()` 回到同一调用；
独立调试确认该 syscall 参数为 `fds=NULL, nfds=0, timeout=NULL, sigmask=NULL`。

## 分析

`kill10.c` 的源码确认上述 `ppoll` 是 `pause()`，而不是 fd readiness 等待。
GDB 也确认 interval timer 会进入 `add_itimer_signal()`，所以硬件时钟和
`alarm()` 设置本身正常。

原实现存在一条依赖调度时序的等待链：

1. `sys_ppoll()` 对无 fd、无限超时的调用只反复设为 Ready 并让出 CPU，未发布
   `Blocked` 等待状态。
2. 这种轮询在 release+CFS 下不能提供可靠的 signal wait hand-off；debug 日志改变
   时间片和交错顺序后才偶然完成。
3. 若等待者正确进入 `Blocked`，`check_blocked_task_timers()` 虽会扫描它，
   `deliver_blocked_itimer_signal()` 却旧地要求存在 `interrupt_waker`。该 waker
   只有 Future/`block_on()` 等待会注册，`ppoll`/`pause` 没有，因此 SIGALRM 不会
   写入 pending 或重新入队。
4. `block_current_and_run_next()` 旧地先从 processor 移走 current task，再写
   `Blocked`。signal 若在两步之间到达会看到 Running，既不入队；随后 task 才变为
   Blocked，形成丢唤醒窗口。

额外审计还发现 raw `ppoll` syscall 的第 4、5 参数 (`sigmask`, `sigsetsize`) 被
忽略，且 pending signal 检查曾在持有 `TaskControlBlockInner` 时读取 `SigTable`，
违反项目锁序。当前 `kill10` 的 `sigmask` 为 NULL，不是这次复现的直接触发条件，
但属于同一等待 syscall 的 Linux ABI/并发缺口，随修复一并收敛。

## 根因

根因是 `pause()` 对应的无 fd `ppoll` 未建立可原子唤醒的 Blocked 状态，而 blocked
ITIMER 投递又错误地仅服务于 Future waker。release 下高频信号流程不再由日志延时
掩盖，最终使 `alarm(1)` 不能形成可靠的 SIGALRM 兜底唤醒，`kill10` 停在等待循环。

## 修复

- `block_current_and_run_next()` 先在 task lock 内发布 `Blocked`，再从当前 processor
  取走 task 并调度。此后 signal 到达要么被等待循环观察到，要么看到 Blocked 并完成
  `Blocked -> Ready` 入队。
- `sys_ppoll()` 对 `nfds == 0 && timeout == NULL` 走上述真正阻塞路径；恢复后重新检查
  pending signal 并返回 `EINTR`，符合 `pause()` 的等待语义。
- `deliver_blocked_itimer_signal()` 不再以 `interrupt_waker` 为前置条件。ITIMER 到期会
  对所有 Blocked task 记录 `SIGALRM` 并在仍为 Blocked 时转为 Ready/入队；Future
  waker 仍会作为额外通知执行。
- 接入 raw `ppoll` 的 `sigsetsize` 第五参数，临时替换 mask 并用 guard 在所有返回路径
  恢复旧 mask；拒绝错误大小并保持 SIGKILL/SIGSTOP 不可屏蔽。
- `ppoll` 先释放 `TaskControlBlockInner` 后查询 `SigTable` action，消除
  `TaskControlBlockInner -> SigTable` 的反向嵌套。

涉及文件：

- `os/src/task/mod.rs`
- `os/src/signal/timer.rs`
- `os/src/syscall/io_mpx/poll.rs`
- `os/src/syscall/mod.rs`
- `user/src/syscall/mod.rs`

## 验证

已执行：

```text
rustfmt --edition 2021 --check os/src/task/mod.rs os/src/signal/timer.rs \
  os/src/syscall/io_mpx/poll.rs os/src/syscall/mod.rs user/src/syscall/mod.rs
git diff --check
make build-arch TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
timeout 120s qemu-system-riscv64 ... -snapshot > log.ans 2>&1
```

结果：

- RISC-V release+CFS QEMU 完整退出，根目录 `log.ans` 中 musl `kill08` 和 `kill10`
  均为 `passed 1 failed 0 broken 0`；`kill10` 输出
  `TPASS: All 2 pgrps received their signals`，最后为 `shutdown!`。
- RISC-V 与 LoongArch64 release 构建均通过；仅有既有 Cargo config 弃用提示和
  vendored `smoltcp` warnings。
- 本轮未重新运行 `make log`，也未运行 LoongArch64 的 `kill10` 行为回归；本次
  `log.ans` 只覆盖维护者当前入口中的 musl `kill08/kill10`。
