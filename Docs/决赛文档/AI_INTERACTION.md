#### open14 O_TMPFILE 匿名临时文件语义修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`O_TMPFILE` 匿名文件语义修复、procfd `readlink/linkat` 兼容、LTP 回归验证协作、文档完善
- **描述**：用户要求分析 `log.ans` 中 `open14` 为什么期望目录参数而不是普通文件，并在通过后写清楚 tmpfile 语义。AI 确认 `O_TMPFILE` 的 path 参数应为目录，但返回 fd 指向未链接的普通文件；原实现把它简化成目录下的真实 `N.tmp` 文件，导致 `readdir()` 看到临时文件。修复新增内存态匿名 `TmpFile`，`openat(O_TMPFILE)` 只校验目录并返回未链接 fd，补齐 `/proc/self/fd/<fd>` 的 `readlinkat()` 与 `linkat()` 物化路径，并修正空目录 `getdents64` EOF 处理。维护者最新 `log.ans` 显示 musl/glibc `open14` 均为 `passed 3 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/open14-o-tmpfile-anonymous.md](./problem/open14-o-tmpfile-anonymous.md)。
- **关联 commit**：待提交

#### copy_file_range02 错误路径语义修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、LTP `format_device` 环境补齐、loop 块设备兼容、`copy_file_range` errno 语义修复、文档完善
- **描述**：用户要求分析 `log.ans` 中 `copy_file_range02` 怎样才能正常进行，并随后提供最新运行结果。AI 先定位到测例在准备阶段因 `$PATH` 中缺少 `mkfs.ext2` 被 `TCONF/skipped`，补齐 busybox applet 链接后又发现 `DevLoop` 缺少 block device `lseek(SEEK_END)` 和容量信息导致 `mkfs.ext2` `TBROK`。补齐 loop seek/容量后，测试进入真正断言阶段，进一步修复 `sys_copy_file_range()` 对 readonly、目录、`O_APPEND`、无效 fd、flags、重叠区间、特殊文件、超大 length、目标范围溢出的 errno。最新 `log.ans` 显示 musl/glibc `copy_file_range02` 均为 `passed 24 failed 0 broken 0 skipped 14`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/copy-file-range02-error-semantics.md](./problem/copy-file-range02-error-semantics.md)。
- **关联 commit**：待提交

#### copy_file_range02 immutable 与 overlap 后续修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、ext2 flags ioctl 兼容、immutable 写保护、同 inode overlap 判断、panic 修复、LTP 回归验证、文档完善
- **描述**：用户提供新的 `log.ans` 和 GDB 回溯，要求继续修复 `copy_file_range02`。AI 确认补齐 `chattr/mkswap/swapon/swapoff` applet 后，immutable file 和 overlapping range 不再 skipped，但 `copy_file_range()` 分别错误成功返回 32/16 字节。修复为在 `OSFile` 支持 `FS_IOC_GETFLAGS/SETFLAGS` 和 `FS_IMMUTABLE_FL` 写保护，并让 `copy_file_range()` 用 `st_dev/st_ino` 判断同一文件重叠范围。用户提供的回溯显示第一次 immutable 检查对通用 `dyn File` 调用默认 `path()` 导致 `EventFd` panic，最终改为确认输出是普通 `OSFile` 后用 `outfile.inode.path()` 检查。最新 `log.ans` 显示 musl/glibc `copy_file_range02` 均为 `passed 26 failed 0 broken 0 skipped 6`，LoongArch/RISC-V 构建均通过。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/copy-file-range02-error-semantics.md](./problem/copy-file-range02-error-semantics.md)。
- **关联 commit**：待提交

#### copy_file_range03 时间戳更新修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`clock_nanosleep` 时间换算修复、`copy_file_range` 写后时间戳语义修复、LTP 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 中 glibc 版 `copy_file_range03` 为什么没有正确更新时间戳。AI 确认真实失败是 glibc 二进制的 `Testing __NR_copy_file_range syscall` raw syscall variant，而 libc wrapper variant 已通过；LTP 用例通过 `fstat(fd_dest).st_mtim` 比较写前写后时间。进一步定位到 `clock_nanosleep()` 将 ticks 换算为微秒时除以 `get_clock_freq()/1000`，导致 1.5 秒等待提前约 1000 倍返回；同时 `copy_file_range()` 写成功后只设置秒级 `mtime`，未保证相对旧值前进、未更新 `ctime` 且吞掉 `set_timestamps()` 错误。修复后 LoongArch 最新 `log.ans` 显示 musl/glibc `copy_file_range03` 均为 `passed 2 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/copy-file-range03-timestamp.md](./problem/copy-file-range03-timestamp.md)。
- **关联 commit**：待提交

#### clone02 CLONE_SIGHAND 退出状态修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：GDB 回溯分析、进程退出/信号共享状态修复、LTP clone 回归验证、文档完善
- **描述**：用户提供 `clone02` 退出路径的 GDB 回溯，显示 `exit_current_and_run_next()` 向父进程发送 `SIGCHLD` 时在 `send_signal_to_thread_group()` 内重入 `ProcessInner::get_locked_sigtable()` 并 panic。AI 定位到根因是 `SigTable` 同时保存 signal action 与 `group_exit_code`；`CLONE_SIGHAND` 父子进程共享 signal action 是正确的，但共享线程组退出码会污染父进程状态，并在子进程退出通知父进程时重入同一把 `sig_table` 锁。修复为把 `group_exit_code` 移入 `ProcessMeta`，`SigTable` 只保留信号处理动作。`make` 通过，最新 `log.ans` 显示 musl/glibc `clone02` 均为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/clone02-shared-sighand-exit-code.md](./problem/clone02-shared-sighand-exit-code.md)。
- **关联 commit**：待提交

#### PCB 与 MemorySet 锁边界重构（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：PCB 结构重构、地址空间锁内聚、锁耦合降低、构建验证、文档完善
- **描述**：用户要求把已经自带同步的 `MemorySet`、`SigTable`、`FdTable`、`FSInfo` 从 `ProcessInner` 中移出，并让 `MemorySet` 自己持有内部锁；后续指出 `personality` 应放入 `ProcessMeta`。AI 将 `MemorySet` 改为内部 `RwLock<MemorySetInner>`，`Process` 改为直接持有自同步资源和可替换 Arc 指针，移除空壳 `ProcessInner`，保留 `inner_lock()` 作为兼容资源快照入口，并重写 fork/exec/exit、用户栈分配、`mremap` 等会触发 `MemorySet` 重入锁的路径。`make` 通过；QEMU 运行验证因沙箱 `/var/tmp` 只读且外部运行被中断，未取得有效运行结果。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：待提交

#### 移除 Process::inner_lock 兼容层（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：PCB 访问接口收敛、`ProcessResources` 兼容层移除、调用点机械迁移、构建验证、决赛文档补充
- **描述**：用户要求继续重构并移除 `inner_lock`。AI 删除 `Process::inner_lock()` 与 `ProcessResources`，在 `Process` 上保留直接资源访问方法，并把 fs、signal、task、io_mpx、net、futex、time 等调用点改成直接访问 `Process` 的地址空间、信号表、fd 表和 fs 上下文；`TaskControlBlock::inner_lock()` 仍保留用于线程级状态。`make` 通过，代码搜索未发现 `Process::inner_lock()` 残留。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：待提交
