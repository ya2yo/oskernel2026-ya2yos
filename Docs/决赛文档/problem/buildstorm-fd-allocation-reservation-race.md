# BuildStorm Rust 子进程 socketpair fd 分配竞态

## 背景

提交 `a9aaf908067c17f8ac6416d8396b67e8d4b0fdb5` 移除了 final 测试入口中单独启动
ArceOS hello-world 的步骤，使 BuildStorm 测试可以连续执行到 Rust 工具链的并发编译
阶段。评测机随后在 Cargo 并发启动 `rustc` 子进程时报告失败。

## 现象

BuildStorm 的第一个编译目标在 Rust 1.98 标准库创建子进程的状态通道时出现：

```text
the CLOEXEC pipe failed: Os { code: 88, message: "Socket operation on non-socket" }
Bad file descriptor (os error 9)
```

日志中的 `CLOEXEC pipe` 是 Rust 标准库沿用的错误文本。Linux 目标上的 Rust 1.98
实际使用 `AF_UNIX/SOCK_SEQPACKET/SOCK_CLOEXEC socketpair` 传递 exec 失败状态；因此
`ENOTSOCK` 表明父进程保存的 socket fd 已经被另一个文件对象覆盖。随后 Cargo worker
因子进程启动失败而退出。

## 分析

内核此前已修复 `socketpair()` 内部连续两次分配 fd 的串行覆盖：先安装第一个端点，
再分配第二个端点。但 `FdTable::alloc_fd()` 仍然只查找空槽并返回编号，调用方要在
后续 `set()` 才安装对象。共享同一 fd 表的多个线程可以按以下顺序交错：

1. 线程 A 调用 `alloc_fd()`，得到空槽 `n`，随后暂时离开 fd 表锁。
2. 线程 B 调用 `alloc_fd()`，仍看到 `n` 为空，也得到同一个编号。
3. A/B 分别 `set(n, socket)`、`set(n, pipe/file)`，后完成者静默替换前一个对象。
4. Rust 父进程读取原 socketpair 状态通道时，fd `n` 已不是 socket，于是返回
   `ENOTSOCK`；在另一种交错下则直接返回 `EBADF`。

该竞态只在同一进程的并发 fd 创建路径出现，单线程回归和低并发的 cagent 测试不一
定触发；Cargo 编译器 worker 的高频 `socketpair/open/pipe/dup` 正好扩大了窗口。

## 根因

fd 表把“空闲槽”和“已分配但尚未安装”的中间状态都表示为 `None`。`alloc_fd()` 与
`set()` 之间没有可被并发调用方观察的 reservation 状态，导致同一 fd 表中的并发
分配可以返回相同编号并互相覆盖。

## 修复

在 `os/src/fs/fstruct.rs` 的 `FdTableInner` 中增加与 `files` 对齐的 `reserved` 位图：

- `alloc_fd()` 和 `alloc_fd_larger_than()` 在 fd 表写锁内查找 `None && !reserved`，
  找到后立即设置 reservation，再返回编号；
- `set()` 安装对象时清除 reservation，并允许提交已经保留的槽；
- `take()` 释放内部失败清理留下的 reservation；
- `resize()`、`clear()` 和 `from_another()` 同步维护位图，未完成的父进程分配不会
  被 clone 到子进程。

这样不需要修改每个 syscall 的两阶段调用协议，就能保证所有现有 fd 分配路径在共享
fd 表的多线程环境中不会重复选槽。fd 表锁仍只保护槽状态，文件对象构造和用户内存
访问不在锁内执行。

## 涉及文件

- `os/src/fs/fstruct.rs`
- `Docs/决赛文档/problem/buildstorm-fd-allocation-reservation-race.md`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

- `make TARGET_ARCH=riscv64`：通过，RISC-V 与 LoongArch64 release 用户态和内核均
  成功构建。
- RISC-V final-2026 镜像：`fstat unlink`、`sigaltstack`、`rseq`、`uptime` 四项回归
  通过，10 项 cagent 全部通过；BuildStorm 已越过原评测失败所在的并发编译路径并
  推进至 `445/446: tg-xtask(bin)`，截至记录时尚未出现 `CLOEXEC pipe`、`ENOTSOCK`
  或 `Bad file descriptor`。
- `git diff --check`：通过。

完整 BuildStorm 结束标记及 LoongArch64 final 运行结果以本轮最终验证为准。
