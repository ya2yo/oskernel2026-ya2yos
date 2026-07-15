# LTP kill10 信号帧 EFAULT 导致内核 panic

## 背景

LTP `kill10` 是高频 `SIGUSR1`/`SIGUSR2` 信号互发测试。它会创建 master、manager 和 child 进程组，并在 handler 内继续发送信号，因此会频繁构造普通 handler 和 `SA_SIGINFO` handler 的用户态 signal frame。

此前已通过 `kill10-siginfo-sender.md` 修复发送者 `siginfo_t.si_pid`，本问题是全量 glibc LTP 中暴露的独立信号帧失败路径。

## 现象

`loongarch.ans` 的 musl LTP `kill10` 已通过；随后 glibc LTP 运行到 `kill10` 时，两个 manager 均完成子进程汇报后出现：

```text
[ERROR] [HART0] [PID 9751] [TID 9751] [setup_frame] save MachineContext should not cause error!
panic
[kernel] Panicked at src/signal/frame.rs:113 explicit panic
```

`PID 9751` 是该轮 `kill10` 的 master。报错位置是普通 handler 保存 `MachineContext` 的第一次 `copy_to_user()`。

## 分析

`copy_to_user()` 是可失败操作：它会检查用户 VMA 的写权限，并在 LoongArch64 上为懒分配页或 COW 页执行 store page-fault 处理。目标地址不属于可写 VMA、页分配/COW 无法完成等情况都会返回 `EFAULT`。

旧 `setup_frame()` 假定保存 `MachineContext`、signal mask、marker、magic word 都“不可能失败”，对任一错误直接 `panic!()`。这把一个当前进程用户栈不能写入的错误扩大为全内核 panic。原日志没有打印失败虚拟地址、目标 VMA 或页表状态，因此无法仅凭该份日志断言 EFAULT 的资源层直接原因。

同时旧实现存在两个确定问题：

- `min_frame_size` 只计入一个 `usize` marker，实际 frame 还会在公共尾部写入第二个 magic word；边界检查少算一个字长。
- 代码持有 `TaskControlBlockInner` 锁时执行用户内存访问，违反 task 模块的锁序约束；`copy_to_user()` 可触发 COW/懒分配，不能在该锁持有期间执行。
- LoongArch `SA_SIGINFO` frame 的有效内容为 840 字节。若原用户栈为 ABI 要求的 16 字节对齐，旧布局会让 handler entry `sp` 变成仅 8 字节对齐；`kill10` master 的 `SA_SIGINFO` handler 会调用 glibc 函数，高频嵌套信号下可能由此破坏用户栈约定。

## 根因

根因是 signal frame 构造将可失败的用户内存写入当作内核不变量，并在 EFAULT 时 `panic!()`。信号栈边界检查不完整、`SA_SIGINFO` frame 未维持 16 字节栈对齐、以及任务锁跨用户内存访问使该路径的健壮性进一步下降。

## 修复

- 计算普通 handler 与 `SA_SIGINFO` handler 的完整 frame 大小，均包含 signal-info marker 和公共 magic word；用 `checked_sub()` 防止栈地址下溢。
- 在 frame 顶端保留必要的对齐填充，保证 handler entry `sp` 按 LoongArch/RISC-V ABI 的 16 字节要求对齐；填充位于 `ucontext` 上方，不改变 `rt_sigreturn` 对 magic、marker、`siginfo` 与 `ucontext` 的既有解析顺序。
- 在更新 trap context 前以 `probe_user_write()` 预检整个 frame 目标范围，确保 VMA 权限、懒分配和 COW 均可完成。
- 任一实际 `copy_to_user()`/`copy_to_user_val()` 失败时记录目标地址，释放引用并终止当前任务为 `SIGSEGV`，不再 panic 内核。
- 先将 trap context 和原 signal mask 拷贝到局部变量，释放 `TaskControlBlockInner` 锁后再访问用户内存；所有 frame 写入成功后才重新加锁提交新的 trap context 和 signal mask。

涉及文件：

- `os/src/signal/frame.rs`

## 验证

已执行：

```text
rustfmt --edition 2021 os/src/signal/frame.rs
make TARGET_ARCH=loongarch64
timeout 120s make run > /tmp/kill10-aligned.log 2>&1
```

结果：

- 根 `Makefile` 实际完成 RISC-V 与 LoongArch64 release 构建；仅有既有 vendored `smoltcp` warning。
- LoongArch64 QEMU 单跑 musl `kill10`：`passed 1 failed 0 broken 0`。
- LoongArch64 QEMU 单跑 glibc `kill10`：`passed 1 failed 0 broken 0`，并正常 `shutdown!`。
- `/tmp/kill10-aligned.log` 中无 `setup_frame` 错误或 kernel panic。

未执行完整 glibc LTP：当前维护者对 `user/src/bin/initproc.rs` 的未提交修改仅保留 musl/glibc `kill10` 单测入口，未恢复全量 LTP。因此，本次验证确认 `kill10` 单测和 panic 防护路径的回归，不宣称已复现或跑完原始全量场景。
