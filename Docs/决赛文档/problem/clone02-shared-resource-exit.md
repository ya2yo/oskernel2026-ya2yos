# LTP clone02 共享资源退出清理修复

## 背景

LTP `clone02` 分别覆盖带 `CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | SIGCHLD` 的共享 clone，以及只带 `SIGCHLD` 的隔离 clone。第一轮要求子进程关闭 fd、修改 cwd、信号 disposition 和地址空间后，父进程可以观察到这些变化；第二轮要求所有变化对父进程隔离。

## 现象

原始 `log.ans` 中 musl 与 glibc 的 `clone02` 都以 wait status `1024` 结束。该值表示测试进程退出码 `4`，即 LTP 的 `TWARN` 位，而非外层打印的 `FAIL LTP CASE` 本身证明了断言失败。glibc 还出现 `write EBADF: fd out of range`，没有输出任何 LTP 断言行。

修复首次无条件清理后，RISC-V 运行能够打印两条 `TPASS`，但包装进程持续等待 pipe EOF，QEMU 在 120 秒后超时。

## 分析

`clone02` 的 LTP 输出由 initproc 的包装进程通过 pipe 收集。共享 clone 子进程退出时，`exit_current_and_run_next()` 无条件执行 `fd_table.clear()` 和 `fs_info.clear()`。因为 `CLONE_FILES` 让子进程和父进程持有同一个 `Arc<FdTable>`，这会关闭父进程仍在使用的描述符，其中包括 stdout/stderr 的 pipe 端，导致 LTP 框架无法输出结果。

仅以 `Arc::strong_count()` 跳过共享对象清理也不正确：已退出的 clone 子进程在被 `wait(2)` 回收前仍以 zombie `Process` 留在 pid 表中并保留 `Arc<FdTable>`。当最后一个实际运行的进程退出时，这个 zombie 引用会阻止 pipe 端关闭，包装进程无法收到 EOF 并永久等待。

因此，`Arc` 的内存生命周期不能表示共享 fd table 和 FS context 的活跃进程所有权。

## 根因

进程退出路径把资源对象清空与 `Process`/zombie 的 `Arc` 生命周期混为一谈：

- 无条件清空会破坏仍在运行的 `CLONE_FILES` / `CLONE_FS` 共享者；
- 仅检查 `Arc` 引用计数会把已退出但尚未 reap 的 zombie 当作活跃共享者。

## 修复

- `FdTable` 与 `FSInfo` 增加独立的原子活跃进程所有者计数，普通新表和复制表初值均为 1；
- 非线程 clone 成功且指定 `CLONE_FILES` 或 `CLONE_FS` 时，分别增加相应共享对象的所有者计数；
- 每个进程组最后一个线程退出时释放一次所有者，只有最后一个活跃所有者才清空 fd 表或 FS context；
- `CLONE_THREAD` 仍共用同一个 `Process`，不增加额外所有者；普通 fork 的复制资源表也保持独立所有权。

## 涉及文件

- `os/src/fs/fstruct.rs`
- `os/src/fs/fs_info.rs`
- `os/src/task/task/task.rs`
- `os/src/task/mod.rs`

## 验证

- `make` 通过，完成 RISC-V 与 LoongArch64 release 构建；仅有既有 vendored `smoltcp` warning。
- `timeout 120s make run > /tmp/clone02-after-fix.log 2>&1` 在 RISC-V QEMU 通过。musl 与 glibc 的 `clone02` 各有两条 `TPASS`，两轮 Summary 均为 `passed 2 failed 0 broken 0 skipped 0 warnings 0`，进程返回状态为 0，最终正常输出 `shutdown!`。
- `timeout 120s make TARGET_ARCH=loongarch64 run > /tmp/clone02-loongarch-after-fix.log 2>&1` 也通过；musl/glibc 两轮均为 `passed 2 failed 0 broken 0 skipped 0 warnings 0`，最终正常 `shutdown!`。启动期的 VirtIO 分配 WARN 为既有日志，测例期间无 WARN、panic、TFAIL 或 TBROK。
