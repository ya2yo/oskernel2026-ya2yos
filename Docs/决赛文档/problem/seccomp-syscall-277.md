# seccomp(277) 系统调用

## 背景

Linux 的 `seccomp(2)` 使用独立的 277 号系统调用安装线程级系统调用限制。项目原先已经通过 `prctl(PR_SET_SECCOMP)` 实现了 strict 模式和受限 classic BPF，但 277 号枚举没有对应的分发入口。

## 现象

用户态直接调用 `seccomp()` 时命中通用未实现分支并返回 `ENOSYS`，无法使用与 `prctl` 等价的 Linux ABI。

## 分析

当前任务已经保存 `SeccompState`，系统调用入口也会在真正执行前解释该状态。`prctl.rs` 中已有安全复制 `struct sock_fprog`、限制过滤器长度和验证 BPF 指令的逻辑，因此 277 号调用可以复用相同的任务状态和 classic BPF 子集，不需要新增过滤器执行器。

## 根因

`Syscall::Seccomp = 277` 只有编号定义，`syscall()` 的 match 未处理该枚举值。

## 修复

- 增加 `sys_seccomp(operation, flags, uargs)`，支持 `SECCOMP_SET_MODE_STRICT` 和 `SECCOMP_SET_MODE_FILTER`。
- 复用现有 `no_new_privs`、`CAP_SYS_ADMIN`、用户地址检查、过滤器校验及线程级 `SeccompState`。
- 当前不支持的 flags、操作码和重复安装按 Linux 语义返回 `EINVAL`，未满足权限要求返回 `EACCES`。
- 接入内核 syscall 分发，并补充用户态 `sys_seccomp` 封装。

## 涉及文件

- `os/src/syscall/mod.rs`
- `os/src/syscall/sys/prctl.rs`
- `user/src/syscall/mod.rs`

## 验证

根目录 `make` 通过，RISC-V 与 LoongArch64 release 内核和用户程序均完成构建。未单独运行 QEMU/LTP 的 277 号直接调用测试。
