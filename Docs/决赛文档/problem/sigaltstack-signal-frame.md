# sigaltstack(2)、SA_ONSTACK 信号帧与 rt_sigreturn 状态恢复

## 背景

BuildStorm 的 `rustup/rustc` 在 RISC-V 用户态调用 Linux syscall `132`，即
`sigaltstack(2)`，并以 `SA_SIGINFO | SA_ONSTACK` 注册信号处理函数。该 ABI 需要线程私有的
备用栈状态、信号帧切栈和 `rt_sigreturn` 恢复协同实现，不能只补一个 syscall 分发号。

## 现象

原始定向启动日志出现 `Unsupported syscall_id: 132`。工具链无法完成其信号运行时初始化，
因而不能继续到后续 `rustc` 路径。

## 根因

内核没有 `sigaltstack(2)` 的 syscall 入口，也没有备用栈在 fork、clone、exec、信号投递和
信号返回间的状态模型。新增初版经复核还发现三个直接的 Linux 可见问题：

1. `rt_sigreturn` 用信号 frame 的 SP 检查替换请求；`SA_ONSTACK` handler 的 frame 必然位于
   备用栈，合法恢复会被错误拒绝为 `EPERM`。
2. `ucontext.uc_stack` 保存的是动态查询值，嵌套 handler 会错误看到 `SS_ONSTACK`；Linux
   保存备用栈的原始配置。
3. 64 位 `stack_t` 的 `flags` 与 `size` 间有四字节 ABI padding，直接拷贝可能把未初始化
   内核字节写给用户态。

## 修复

- 在 `SignalStack` 中维护 `sp`、flags、size 和显式清零的 ABI padding；实现
  `SS_DISABLE`、`SS_ONSTACK`、`SS_AUTODISARM`、最小栈长及参数校验。RISC-V 的
  `MINSIGSTKSZ` 为 2048，LoongArch64 为 4096。
- 在 `os/src/syscall/mod.rs` 接入 syscall `132`，`sys_sigaltstack()` 使用
  `copy_from_user` / `copy_to_user`，并把用户内存访问放在 task 锁外。
- 备用栈状态放入 `TaskControlBlockInner`：普通 fork 继承，`CLONE_VM` 线程清空，exec 清空。
- `SA_ONSTACK` 从 `ss_sp + ss_size` 向下创建 frame，嵌套 frame 保持在当前备用栈；
  `ucontext.uc_stack` 和普通 handler frame 保存原始 `SignalStack` 配置。
- `rt_sigreturn` 先恢复 `MachineContext`，再以恢复后的被中断用户 SP 校验并恢复备用栈，
  与 Linux RISC-V/LoongArch 处理顺序一致。
- 用户态 `StackT` 同步显式 padding，并在 `initproc` 增加基础回归。

涉及文件：

- `os/src/signal/types.rs`
- `os/src/signal/frame.rs`
- `os/src/syscall/mod.rs`
- `os/src/syscall/signal.rs`
- `os/src/task/task/task.rs`
- `user/src/lib.rs`
- `user/src/syscall/mod.rs`
- `user/src/bin/initproc.rs`
- `user/src/bin/initproc/sigaltstack_regression.rs`

## 验证

本次主补丁阶段的 RISC-V debug 启动日志已输出 `sigaltstack regression: PASS`，并且不再报
`Unsupported syscall_id: 132`。最终整理后的 RISC-V、LoongArch64 release 均构建通过。

现有回归覆盖初始 query、设置、`SA_SIGINFO | SA_ONSTACK` handler、handler 栈位置、
`ucontext`、disable、`ENOMEM` 和 `EINVAL`。它尚未覆盖 handler 篡改 frame 的坏帧恢复路径，
该既有 `rt_sigreturn` 健壮性问题未在本轮扩大处理；完整 BuildStorm 行为回归也待续跑。
