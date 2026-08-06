# System V 消息队列 syscall 实现

## 背景

Ya2yOS 已有共享内存等 IPC syscall，但缺少 Linux 用户程序和 LTP 常用的 System V 消息队列接口。消息队列不是文件描述符，应该由内核维护全局对象，并通过 Linux 约定的消息缓冲区和 `msqid_ds` ABI 与用户态交互。

## 现象

调用 `msgget`、`msgsnd`、`msgrcv` 或 `msgctl` 时没有对应内核分发和用户态封装，消息队列测试无法创建队列、发送/接收消息或查询队列状态。

## 分析

消息队列需要同时处理三类边界：队列对象生命周期和 key 查找、消息类型选择与队列容量、阻塞任务与 `IPC_RMID`/信号的并发唤醒。用户指针还分为固定布局和变长正文：消息类型以及 `msgctl` 的固定结构适合使用 `copy_from_user_val`/`copy_to_user_val`，正文则使用字节切片复制，避免在未知长度数据上构造不安全的 Rust 类型。

RISC-V 与 LoongArch64 的 `ipc_perm` 布局不同：前者的 `mode/seq` 是 16 位字段并带显式填充，后者使用 32 位 `mode` 和 32 位 `seq`。内核侧分别定义 `repr(C)` 用户 ABI 结构，不能直接把内部锁保护状态暴露给用户。

## 根因

缺失 syscall 号、分发入口、队列管理器和用户态 wrapper，导致上述 ABI 和并发语义完全未实现。若在阻塞等待期间持有队列锁，或者先注册 waker 后不重新检查状态，还会产生锁跨调度和丢唤醒风险。

## 修复

- 新增全局 `MsgManager`、`MsgQueue` 和 `QueueState`，以 key 映射非 `IPC_PRIVATE` 队列，以 `Arc + Mutex` 管理消息、容量、权限、时间戳及最近发送/接收进程。
- 接入 Linux syscall 号 186--189 及 `sys_msgget`、`sys_msgsnd`、`sys_msgrcv`、`sys_msgctl` 分发；用户库增加对应薄封装。
- 实现 `IPC_PRIVATE/IPC_CREAT/IPC_EXCL/IPC_NOWAIT`、`IPC_STAT/IPC_SET/IPC_RMID`，以及 `IPC_INFO/MSG_INFO/MSG_STAT/MSG_STAT_ANY` 查询；实现正、负、零消息类型选择和 `MSG_NOERROR/MSG_EXCEPT/MSG_COPY`。
- 发送前复制固定消息类型和正文，接收后复制类型与正文；定长 `msgctl` 结构使用 value copy helper，所有队列锁在用户内存复制和阻塞调度前释放。
- 队列满或目标类型暂不存在时使用 `PollSet` 阻塞等待并二次检查；接收释放容量后唤醒发送者，发送加入消息后唤醒接收者；`IPC_RMID` 标记队列并唤醒双方，阻塞调用返回 `EIDRM`，中断返回 `EINTR`。
- 新增 `user/src/bin/initproc/msg_regression.rs`，覆盖创建、普通收发、负类型选择、`MSG_COPY`、`MSG_NOERROR`、`IPC_STAT/IPC_SET`、`IPC_NOWAIT`、满队列阻塞和删除唤醒。

## 涉及文件

- `os/src/syscall/ipc/msg.rs`
- `os/src/syscall/ipc/mod.rs`
- `os/src/syscall/mod.rs`
- `user/src/syscall/mod.rs`
- `user/src/lib.rs`
- `user/src/bin/initproc/msg_regression.rs`
- `user/src/bin/initproc.rs`

## 验证

- `make TARGET_ARCH=riscv64 build-arch`：通过。
- `make TARGET_ARCH=loongarch64 build-arch`：通过；仅有既有 `smoltcp` 未使用项警告。
- `rustfmt --edition 2021 --check`（消息队列相关文件）：通过。
- `git diff --check`：通过。
- RISC-V QEMU 直接执行消息队列回归：输出 `msg regression passed` 和 `shutdown!`，覆盖上述功能及阻塞唤醒路径。
- 尝试运行磁盘镜像内 musl LTP `msg*` 测例时，镜像缺少对应 `/musl/ltp/testcases/bin/msg*` 可执行文件，均为 `execve fail`，因此不能据此判断 syscall 失败；完整 LTP 仍待使用包含这些二进制的测试镜像验证。
