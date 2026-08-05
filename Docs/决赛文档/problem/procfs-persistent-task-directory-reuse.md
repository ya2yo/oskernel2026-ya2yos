# 持久化镜像下 procfs 任务目录复用导致启动 panic

## 背景

QEMU 使用持久化 guest 镜像时，上一轮运行留下的 EXT4 文件不会因重启自动清除。内核在创建 initproc 的 `/proc/<pid>` 条目时需要兼容这种残留状态，同时保持 Linux 的目录打开语义。

## 现象

新的 `server.ans` 报告：

```text
[kernel] Panicked at src/task/task/task.rs:582 create initproc proc files: EISDIR
```

`client.ans` 的 GDB 栈停在 `TaskControlBlock::new()` 的 `create_proc_dir_and_file(...).expect(...)`。因此 panic 发生在 initproc 启动阶段，还没有进入后续测试。

## 分析

旧实现每次都用 `O_CREATE | O_DIRECTORY | O_RDWR` 打开 `/proc/<pid>`。当持久化镜像中已经存在该目录时，`open_inner()` 按 Linux 语义把可写目录打开拒绝为 `EISDIR`，错误被 `expect()` 转成内核 panic。即使目录本身可复用，旧的 `stat`、`status`、`maps` 和 `pagemap` 普通文件内容也可能来自上一轮进程，不能直接当作当前任务状态。

## 根因

目录创建路径同时承担了“探测已有目录”和“创建新目录”两种操作，却使用了带写意图的 `O_CREATE | O_RDWR`。在既有目录上这不是 Linux 的复用操作，而是一个应返回 `EISDIR` 的非法打开请求。

## 修复

- `create_proc_dir_and_file()` 先以只读 `O_DIRECTORY` 探测 `/proc/<pid>`；探测成功则复用目录，仅在 `ENOENT` 时以 `O_CREATE | O_DIRECTORY` 创建。
- 探测得到其他错误继续向上传播，避免把权限、类型或文件系统错误误判为“目录不存在”。
- `stat`、`status`、`maps`、`pagemap` 均以 `O_TRUNC` 打开，覆盖持久化镜像中的旧内容；`stat` 的可失败打开不再使用 `unwrap()`。

## 涉及文件

- `os/src/fs/kernel_fs_ops/proc_file.rs`

## 验证

- `make TARGET_ARCH=riscv64`：通过。
- `make TARGET_ARCH=loongarch64`：通过。
- 在保留旧 procfs 条目的持久化 RISC-V 镜像上运行 `timeout 45 make TARGET_ARCH=riscv64 run`：不再出现 `panic`、`EISDIR` 或 `create initproc proc files`，日志越过 initproc 启动、`sigaltstack regression: PASS`、`rseq regression: PASS`、`BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok` 和 `pre-build tg-xtask`。
- 该 45 秒窗口在正式 BuildStorm 编译期间超时，未完成 446 crate 全量 BuildStorm、LTP、fsck 或 LoongArch64 QEMU 运行。

