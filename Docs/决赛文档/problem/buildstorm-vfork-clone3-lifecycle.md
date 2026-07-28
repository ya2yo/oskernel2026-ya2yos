# BuildStorm clone3/vfork 共享地址空间交接

## 背景

final-2026 的 Rust toolchain 会通过 `clone3()` 创建 pthread，也会以
`CLONE_VM | CLONE_VFORK | CLONE_CLEAR_SIGHAND` 启动 `rustc`。这类子进程在
`execve()` 或退出前与父进程共享地址空间，调度、页表归属和父子唤醒顺序必须满足
`vfork(2)` 的交接约束。

## 现象

根目录 `log.ans` 已记录两次 Rustup/Cargo vfork 调用：

```text
PID 8/TID 8 clone3 -> CLONE_VM | CLONE_VFORK | CLONE_CLEAR_SIGHAND
PID 10/TID 10 execve -> .../bin/rustc -vV

PID 8/TID 8 clone3 -> CLONE_VM | CLONE_VFORK | CLONE_CLEAR_SIGHAND
PID 12/TID 12 execve -> .../bin/rustc --print=file-names ...
```

旧路径虽然会在 `clone_process()` 中把父 task 标为 `VforkBlocked`，但 syscall
仍可能先返回父用户态；同时 `exec()` 在新页表、用户栈和 trap context 安装前就唤醒父
task。二者都会让父子在仍共享的旧地址空间中并发执行，违背 vfork 的基本约束。

RISC-V SMP 下，非线程 `CLONE_VM` 子进程还会经 `Process::new()` 按 pid 分配到另一个
hart。内核目前没有 remote TLB shootdown，两个 hart 并发使用同一 `MemorySet` 时不能保证
页表更新可见。

此外，`clone3_args.stack` 是栈底，`stack_size` 是长度，而 task 核心接收的是栈顶。旧代码
只在 LoongArch64 做 `stack + stack_size` 换算，使 RISC-V 的 clone3 pthread/vfork 栈 ABI
不一致。

## 根因

1. `CLONE_VFORK` 的状态转换与实际调度切换脱节，父 task 已标记阻塞却仍可从
   `sys_clone()` 返回。
2. exec 路径过早解除父 task 的 vfork 等待，子 task 尚未完成共享地址空间替换。
3. 非线程 `CLONE_VM` 创建独立 `Process` 时丢失父进程的 `home_hart` 约束。
4. clone3 的 stack ABI 被错误地按架构区分。
5. `exit_group()` 用于清理 sibling 的内部 `SIGKILL` 会覆盖已选定的正常 group exit
   status；exec de-thread 完成后也需要清除这类内部清理信号留下的状态。

## 修复

- `sys_clone()` 在子 task 入队后，对 `CLONE_VFORK` 立即调用
  `suspend_current_and_run_next()`。父 task 保持 `VforkBlocked`，直到子 task exec 成功或
  走既有退出唤醒路径。
- `TaskControlBlock::exec()` 将 vfork 父唤醒移到新 `MemorySet`、用户栈和 trap context
  全部安装后；这样父 task 被重新入队时，子 task 已不再使用旧共享地址空间。
- 为 `Process` 提供 `new_on_hart()`，非线程 `CLONE_VM` 子进程继承父进程的
  `home_hart`，避免在尚无 remote TLB shootdown 的实现中跨 hart 共享页表。
- 对全部架构统一按 `clone3_args.stack + stack_size` 生成 legacy clone 所需的栈顶，并保留
  零 stack/非零 size 的 `EINVAL` 校验。
- group 已有 `group_exit_code` 时，内部 `SIGKILL` 不再覆盖 `termination_signal`；exec
  de-thread 收敛 sibling 后清除其临时记录。

## 涉及文件

- `os/src/syscall/task/clone.rs`
- `os/src/syscall/task/clone3.rs`
- `os/src/task/task/task.rs`
- `os/src/task/process/process.rs`
- `os/src/signal/pending.rs`

## 验证

已执行：

```text
make build-arch TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
```

两架构 release 构建均通过。现有 RISC-V `log.ans` 已显示上述两次
`clone3(CLONE_VM | CLONE_VFORK)` 后由子 task 进入对应的 `rustc execve`，且没有 panic。

本轮 `timeout 90s make run > /tmp/vfork-clone3-commit.log 2>&1` 能完成启动、输出
`sigaltstack regression: PASS` 与 `rseq regression: PASS`，并进入
`buildstorm_xtask_prebuild_debug.sh`。外层时限在 Rust 编译和目标 vfork 前结束，采集范围内
没有 panic、`TFAIL` 或 `TBROK`；该样本不作为完整 vfork 或 BuildStorm 行为通过结论。

本次不将完整 BuildStorm 标为通过。最新 fresh MINIBUILD 的 `Bad address (os error 14)`
已确认来自 Cargo worker 对普通 `clone()` 的调用：fork 克隆时遗漏动态 `MAP_STACK` VMA，
随后写 `CLONE_CHILD_SETTID` 失败。该独立的 fork 地址空间复制问题不在本提交范围内。

## 2026-07-28：长 vfork 等待的分段时序

### 现象与目标

维护者提供的 `full.ans` 在 `[101/446]` 后约 61 分钟没有 Cargo 进度。同期
`clone_duration vfork_wait` 从 `93 samples / 1,207,906,826us` 增至
`95 samples / 3,772,493,137us`，单次最大 `2,525,349,969us`（约 42 分钟）。
这段窗口只有约 `10.75 MiB` EXT4 读取，且已记录的 `execve` 最大时长约 234 秒，
不足以直接解释 42 分钟的 parent wait。

因此本轮不提前唤醒 parent，也不把问题武断归为 ELF 装载；先把 vfork hand-off
拆为可证伪的时序边界，以区分 child 未及时运行、exec 提交缓慢、ready queue 延迟，
以及 child 走 exit 或 SIGKILL 兜底的情形。

### 修改

原有 `clone_duration vfork_wait` 保留为 parent 端到端等待。`perf` feature 下的
`TaskControlBlockInner` 另保存三个时间戳，全部在既有 task inner lock 保护下读写：

- `child_to_exec`：child 发布到首次进入 `sys_execve()`；
- `exec_to_parent_ready`：首次进入 `sys_execve()` 到新地址空间、用户栈与 trap context
  安装完成后，child 将 parent 设为 `Ready`；
- `child_to_exit`：没有通过 exec 解除等待，而由 child exit 解除 parent 时的 child 发布到
  parent `Ready`；
- `parent_ready_to_resume`：parent 被设为 `Ready` 到其原 `sys_clone()` 的
  `suspend_current_and_run_next()` 返回。

报告还输出 `vfork_release exec=<n> exit=<n> signal=<n>`。`signal` 仅统计现有的
`SIGKILL` 将 `VforkBlocked` parent 恢复为 `Ready` 的兜底路径；它不被视为正常 exec
完成。`exec`、`exit` 和 `signal` 均只在实际完成该 `VforkBlocked -> Ready` 转换时递增，
避免重复归因。

时间戳记录和聚合均是 perf 构建中的常数时间字段赋值及 relaxed atomic，不输出逐 clone
日志，也不改变地址空间提交、task state 或 ready queue 的既有顺序。

### 验证与边界

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
make build-arch TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
```

两个架构的 perf 与普通 release 构建均通过，只有既有的 smoltcp unused-item warning。
本轮未重新运行接近四小时的 BuildStorm；短样本无法覆盖 `[101/446]` 后的异常窗口，因而尚无
四段时序 guest 数据，不能据此报告端到端加速。
