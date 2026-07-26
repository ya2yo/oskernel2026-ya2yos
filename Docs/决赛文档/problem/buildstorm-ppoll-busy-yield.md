# BuildStorm `ppoll` 忙让出导致调度器热循环

## 背景

维护者将 `initproc` 定向到 `buildstorm::compile::run()`，以 RISC-V perf 内核运行
`cargo build -p tg-xtask`。该入口通过 Bash 管道连接 Cargo 与 `tee`，Cargo 会使用
`ppoll(2)` 等待管道状态和进度事件。

## 现象

旧 `log.ans` 中，Cargo 在约 382 s 仅推进到 `4/446`，而调度器累计
`selections=109240503`、`self_selections=66149241`。运行至约 1196 s 时，调度选择达到
`399314944` 次；同期系统调用总数只有约 92 万次，EXT4 读取和锁操作的增量也远小于调度
循环次数。因此这不是单纯的文件系统吞吐问题。

## 分析

`sys_ppoll()` 每次扫描 fd 未就绪后直接调用
`suspend_current_and_run_next()`。该调用将当前任务标记为 `Ready`，调度器可以立即再次选中
同一任务，随后它在同一个 `ppoll` syscall 内重新扫描 fd。Cargo 等待 pipe 时由此形成
“扫描 -> Ready -> 切换 -> 再扫描”的内核态忙让出循环。

已有 `pselect6` 路径已经具备文件 `register()` waker、`block_on()` 与 timer future 的等待
模型，可用于证明 fd 就绪、相对超时和任务阻塞能够正确协作。`ppoll` 的无 fd 无限等待此前
虽有专门的阻塞分支以支持 `pause()`，但带 fd 或有限超时仍保留了忙让出实现。

## 根因

`ppoll` 把“暂时没有就绪 fd”错误实现为一次协作让出，而没有发布 `Blocked` 状态和注册文件
waker。在存在其他可运行 Cargo 任务时，CFS 会不断重新选择该等待者，消耗大量调度、上下文
切换和锁操作，延缓真正编译任务运行。

## 修复

- 将 `sys_ppoll()` 改为快照一次 fd table，持有 `Arc<dyn File>`，避免每次被唤醒后重复取得
  fd table。
- 每轮先执行一次 `poll()`；无事件时为有效 fd 注册 waker，再二次扫描以覆盖“首次扫描与注册
  之间发生事件”的窗口。
- 使用 `block_on(timeout(...))` 休眠至文件事件、可见信号或相对 timeout 到来；保留临时
  signal mask guard 和忽略信号的消费语义。
- `timeout = {0, 0}` 现在仍完成一次就绪扫描并写回 `revents`，无效 fd 返回 `POLLNVAL`；旧实现
  会错误地直接返回 0。

## 涉及文件

- `os/src/syscall/io_mpx/poll.rs`
- `Docs/决赛文档/problem/buildstorm-ppoll-busy-yield.md`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

- `rustfmt --edition 2021 os/src/syscall/io_mpx/poll.rs` 与 `git diff --check`：通过。
- `make perf TARGET_ARCH=riscv64`：通过，仅有既有 smoltcp warning。
- `make build-arch TARGET_ARCH=loongarch64`：通过，仅有既有 smoltcp warning。
- RISC-V `buildstorm::compile::run()` 新 `log.ans`：运行至 guest `341684 ms` 时，Cargo 已到
  `6/446`，调度器仅 `selections=153080`、`self_selections=124709`；无 `panic`、`TFAIL` 或
  `TBROK`。与旧样本相近窗口的约 9500 万次选择相比，热循环已消除。
- QEMU 被外层终止，日志以 `QEMU: Terminated` 结束，尚未得到 `BUILDSTORM_DEBUG_COMPILE ok`
  或完整 446 crate wall-clock，不能据此宣称完整 BuildStorm 编译通过或给出正式评分加速比例。
- 新样本最终仍有 EXT4 锁累计等待约 42.8 s；这是消除调度热循环后暴露的下一层瓶颈，未在本轮
  扩大修改范围。
