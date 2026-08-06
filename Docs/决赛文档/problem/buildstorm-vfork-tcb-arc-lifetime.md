# BuildStorm vfork 子 TCB 强引用生命周期

## 背景

BuildStorm 通过 `clone(CLONE_VM | CLONE_VFORK)`/`clone3()` 创建工具链 worker。vfork
父线程在子线程执行 `execve()` 或退出前会留在内核调度路径中。

## 现象

最新 `server.ans` 在 `buildstorm` 阶段连续报告多个
`exit_current_and_run_next` warning：不同 Hart 上的 worker 退出时
`strong_count = 4`。随后日志停在 `pre-build tg-xtask` 并出现 kernel trap；
`client.ans` 只有 GDB 连接和 panic breakpoint，未提供更早的用户态栈。

## 分析

all-hart CFS 就绪队列只保存 `Weak<TaskControlBlock>`，TID 表保存一个强引用，
当前处理器槽和退出路径局部变量构成退出时的正常引用。`sys_clone()` 在创建
vfork 子线程后把 `new_task: Arc<TaskControlBlock>` 带过
`suspend_current_and_run_next()`；父线程挂起期间，这个局部变量仍位于父线程的
内核栈上，额外持有子 TCB。子线程退出时该引用仍存在，因此稳定观察到多一个强引用。

## 根因

`new_task` 只需要在入队和读取 TID 时短暂持有。vfork 等待期间继续保留它违反了
“就绪队列弱引用、TID 表负责生命周期”的所有权约定，导致子 TCB 退出时引用计数
异常，并延迟 TCB/内核栈回收。

## 修复

在 `sys_clone()` 完成 `ready_queue::add_task()` 和 `new_tid` 提取后立即
`drop(new_task)`，再进入 vfork 挂起路径。子 TCB 生命周期由 TID 表和当前调度器
引用管理，父线程只保留等待状态，不再持有子 TCB 强引用。

## 涉及文件

- `os/src/syscall/task/clone.rs`

## 验证

- `make TARGET_ARCH=riscv64`：RISC-V 与默认流程中的 LoongArch64 release 构建通过。
- `git diff --check`：通过。
- `timeout 180s make TARGET_ARCH=riscv64 run` 及同命令使用 `make perf TARGET_ARCH=riscv64`
  构建的内核：启动 8 个 Hart，完成
  `sigaltstack`/`rseq` 回归并进入 `BUILDSTORM_TOOLCHAIN ok`；修复后输出没有
  `strong_count` 或 `extra TCB refs`。随后仍出现 `spin::Lazy`/`spin::Once` poisoned panic，测试
  在 `BUILDSTORM_MINIBUILD` 后未完成，因此不能据此宣称完整 BuildStorm 端到端通过。
