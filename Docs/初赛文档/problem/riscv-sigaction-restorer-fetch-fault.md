# RISC-V rt_sigaction restorer 丢失导致取指 0 地址

## 背景

在 LoongArch `lmbench-musl` 修复后，切回 RISC-V 架构运行 `make run`，系统在进入第一组 `basic-musl` 后很快卡住。用户要求优先处理当前 `log.ans` 中的 RISC-V `FetchInstructionPageFault`。

相关启动路径是 `initproc` fork 出 PID 2，随后 PID 2 `execve("busybox", ["busybox", "sh", "basic_testcode.sh"])` 运行 busybox shell。此前优化 iperf 前 lmbench 正常运行，因此这次优先确认是否是 recent signal/exec ABI 改动影响了 RISC-V。

## 现象

`log.ans` 中在打印：

```text
initproc running......
["busybox\0", "sh\0", "basic_testcode.sh\0"]
#### OS COMP TEST GROUP START basic-musl ####
```

之后，PID 2 连续输出：

```text
Exception(FetchInstructionPageFault) in application, bad addr = 0x0, bad instruction = 0x0, sending SIGSEGV.
```

其中日志里的 `bad instruction` 实际打印的是当前 trap context 的 `sepc`。因此可判断用户态正在从 PC 0 取指。

## 分析

最初怀疑点包括 fork 子进程 trap context 复制错误、exec 入口地址为 0、ELF loader 返回错误入口、以及 SIGSEGV 默认动作没有终止进程。临时诊断输出显示：

- PID 2 fork 后 `sepc=0x100b8`，不是 0。
- PID 2 成功进入 `execve("busybox")`，`TaskControlBlock::exec()` 给 busybox 设置的 entry 为 `0x10148`。
- busybox 后续能继续执行，并进入 `basic-musl` 组，说明 exec/ELF 初始化不是根因。
- 取指 0 前，PID 2 的 trap context 曾被设置为 `sepc=0x7fa78, ra=0x0`。

继续在信号路径加窄范围诊断后，确认 PID 2 注册了 SIGCHLD handler：

```text
raw_handler=0x7fa78 raw_flags=0x4000000
```

`0x4000000` 是 `SA_RESTORER`。随后内核投递 SIGCHLD 时执行 `setup_frame()`，把 signal handler 设置为 `0x7fa78`，但由于内部 `SigAction.sa_restore` 为 0，最终把 handler 的返回地址 `ra` 设为 0。SIGCHLD handler 正常返回后执行 `ret`，于是跳转到 0 地址取指。

## 根因

RISC-V 用户态传给 `rt_sigaction` 的 raw action 布局包含 `sa_restorer`：

```text
handler, flags, restorer, mask
```

但内核此前为了修 LoongArch/musl，把 `rt_sigaction` syscall 边界统一解析为：

```text
handler, flags, mask[2], unused
```

这个布局适用于当前 LoongArch 测试路径，但不适用于 RISC-V。RISC-V musl/glibc 设置 `SA_RESTORER` 时，内核没有保存用户提供的 restorer 地址，`setup_frame()` 看到 `SA_RESTORER` 后使用 `sa_restore=0`，导致 signal handler 返回到 0。

SIGSEGV 之后仍反复打印，是同一个错误的次生现象：SIGSEGV 也可能被当前 signal mask 阻塞，或还没完成默认终止前就继续回到坏的 `sepc=0` 路径。主因仍是最早的 SIGCHLD handler 返回地址错误。

## 修复

按架构拆分 syscall 边界 raw ABI：

- RISC-V：`RawSigAction { handler, flags, restorer, mask }`，转换为内部 `SigAction` 时保留 `sa_restore`。
- 非 RISC-V：保持已有 `RawSigAction { handler, flags, mask[2], unused }`，避免影响已经验证过的 LoongArch `lmbench-musl`。
- 同步 `user/src/lib.rs` 中测试用户库的 `RawSigAction` 布局，保证内核自带用户程序与 syscall ABI 一致。

涉及文件：

| 文件 | 修改 |
|------|------|
| `os/src/syscall/signal.rs` | 为 RISC-V 增加带 `restorer` 字段的 raw `rt_sigaction` 布局，并转换到内部 `SigAction.sa_restore` |
| `user/src/lib.rs` | RISC-V 用户库 `RawSigAction` 增加 `sa_restore` 字段；非 RISC-V 保持原布局 |

## 验证

已执行：

```text
make
timeout 80s make run > log.ans 2>&1
```

结果：

- `make` 通过。
- 修复后 80 秒运行未再出现 `FetchInstructionPageFault` 或 `bad addr = 0x0`。
- 日志显示已越过原始卡点，连续输出：

```text
#### OS COMP TEST GROUP END basic-musl ####
#### OS COMP TEST GROUP END basic-glibc ####
#### OS COMP TEST GROUP END busybox-musl ####
#### OS COMP TEST GROUP END busybox-glibc ####
#### OS COMP TEST GROUP END lua-musl ####
#### OS COMP TEST GROUP END lua-glibc ####
#### OS COMP TEST GROUP END iperf-musl ####
#### OS COMP TEST GROUP END iperf-glibc ####
#### OS COMP TEST GROUP END cyclictest-musl ####
#### OS COMP TEST GROUP END cyclictest-glibc ####
```

本次 80 秒窗口在 `libctest-musl` 中被 timeout 截断，尚未验证完整 `make run` 全量自然结束。
