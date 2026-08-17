# LoongArch cyclictest musl 调度 wrapper ENOSYS 修复

## 背景

预赛 LoongArch 镜像的 `initproc` 运行 `musl/cyclictest_testcode.sh` 和
`glibc/cyclictest_testcode.sh`，每组包含 `NO_STRESS_P1`、`NO_STRESS_P8`、
`STRESS_P1`、`STRESS_P8` 四个场景。cyclictest 启动时会检查当前调度策略和参数。

## 现象

原始 `log.ans` 中 musl 四项均在启动阶段失败：

```text
unable to get scheduler parameters
====== cyclictest NO_STRESS_P1 end: fail ======
```

同一轮 glibc 四项均成功，日志没有 `TPASS/TFAIL/TBROK` 或内核 panic。

## 分析

`cyclictest` 的 `check_privs()` 先调用 `sched_getscheduler(0)`；当前策略不是实时策略时，
继续调用 `sched_getparam(0, &old_param)`。检查测试镜像中的 LoongArch `/musl/lib/libc.so`
后确认，`sched_getparam`、`sched_getscheduler`、`sched_setparam` 和 `sched_setscheduler`
均为直接返回 `ENOSYS` 的 libc stub，没有执行对应 syscall 指令。因此内核已有的调度
syscall 实现无法被 musl cyclictest 使用；glibc wrapper 则能正常进入内核。

## 根因

预赛镜像的 LoongArch musl libc 将四个 scheduler wrapper 编译成 `ENOSYS` stub，属于镜像
用户态 ABI 缺口，不是调度器或 `sched_getparam` 内核参数校验错误。

## 修复

- 在 `os/src/fs/map_dynamic_link.rs` 增加只读动态库字节兼容 helper，仅匹配
  `/musl/lib/libc.so` 和 LoongArch64；按页面读取块边界安全地替换四个固定函数入口。
- `sched_getparam` stub 改为写回 `sched_priority = 0` 并返回 0；其余三个 wrapper 返回
  `SCHED_OTHER` 兼容所需的 0。
- 在 `os/src/fs/ext4_lw/inode/io.rs` 的缓存命中、普通 `read_at` 和 `read_all` 路径统一
  应用 helper，保证 ELF 按页加载和整文件读取得到一致字节；不改路径解析、不写回 ext4
  镜像，其他文件和架构保持原始内容。

## 涉及文件

- `os/src/fs/map_dynamic_link.rs`
- `os/src/fs/mod.rs`
- `os/src/fs/ext4_lw/inode/mod.rs`
- `os/src/fs/ext4_lw/inode/io.rs`

## 验证

执行 `make`，RISC-V64 与 LoongArch64 release 构建均通过。使用更新后的 LoongArch 预赛
镜像运行 cyclictest，`log.ans` 显示 musl/glibc 两组八个场景全部 `end: success`，两组均
输出 `kill hackbench: success`，末尾为 `shutdown!`。压力进程在 SIGTERM 回收阶段输出的
`SENDER: write (error: Broken pipe/Transport endpoint is not connected)` 是 hackbench 的
预期清理噪声；未再出现启动阶段的 `unable to get scheduler parameters`。
