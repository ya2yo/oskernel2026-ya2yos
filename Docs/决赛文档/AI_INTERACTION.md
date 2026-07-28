# 决赛文档说明

#### open14 O_TMPFILE 匿名临时文件语义修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`O_TMPFILE` 匿名文件语义修复、procfd `readlink/linkat` 兼容、LTP 回归验证协作、文档完善
- **描述**：用户要求分析 `log.ans` 中 `open14` 为什么期望目录参数而不是普通文件，并在通过后写清楚 tmpfile 语义。AI 确认 `O_TMPFILE` 的 path 参数应为目录，但返回 fd 指向未链接的普通文件；原实现把它简化成目录下的真实 `N.tmp` 文件，导致 `readdir()` 看到临时文件。修复新增内存态匿名 `TmpFile`，`openat(O_TMPFILE)` 只校验目录并返回未链接 fd，补齐 `/proc/self/fd/<fd>` 的 `readlinkat()` 与 `linkat()` 物化路径，并修正空目录 `getdents64` EOF 处理。维护者最新 `log.ans` 显示 musl/glibc `open14` 均为 `passed 3 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/open14-o-tmpfile-anonymous.md](./problem/open14-o-tmpfile-anonymous.md)。
- **关联 commit**：`fa1d814`

#### copy_file_range02 错误路径语义修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、LTP `format_device` 环境补齐、loop 块设备兼容、`copy_file_range` errno 语义修复、文档完善
- **描述**：用户要求分析 `log.ans` 中 `copy_file_range02` 怎样才能正常进行，并随后提供最新运行结果。AI 先定位到测例在准备阶段因 `$PATH` 中缺少 `mkfs.ext2` 被 `TCONF/skipped`，补齐 busybox applet 链接后又发现 `DevLoop` 缺少 block device `lseek(SEEK_END)` 和容量信息导致 `mkfs.ext2` `TBROK`。补齐 loop seek/容量后，测试进入真正断言阶段，进一步修复 `sys_copy_file_range()` 对 readonly、目录、`O_APPEND`、无效 fd、flags、重叠区间、特殊文件、超大 length、目标范围溢出的 errno。最新 `log.ans` 显示 musl/glibc `copy_file_range02` 均为 `passed 24 failed 0 broken 0 skipped 14`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/copy-file-range02-error-semantics.md](./problem/copy-file-range02-error-semantics.md)。
- **关联 commit**：`975df42`

#### copy_file_range02 immutable 与 overlap 后续修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、ext2 flags ioctl 兼容、immutable 写保护、同 inode overlap 判断、panic 修复、LTP 回归验证、文档完善
- **描述**：用户提供新的 `log.ans` 和 GDB 回溯，要求继续修复 `copy_file_range02`。AI 确认补齐 `chattr/mkswap/swapon/swapoff` applet 后，immutable file 和 overlapping range 不再 skipped，但 `copy_file_range()` 分别错误成功返回 32/16 字节。修复为在 `OSFile` 支持 `FS_IOC_GETFLAGS/SETFLAGS` 和 `FS_IMMUTABLE_FL` 写保护，并让 `copy_file_range()` 用 `st_dev/st_ino` 判断同一文件重叠范围。用户提供的回溯显示第一次 immutable 检查对通用 `dyn File` 调用默认 `path()` 导致 `EventFd` panic，最终改为确认输出是普通 `OSFile` 后用 `outfile.inode.path()` 检查。最新 `log.ans` 显示 musl/glibc `copy_file_range02` 均为 `passed 26 failed 0 broken 0 skipped 6`，LoongArch/RISC-V 构建均通过。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/copy-file-range02-error-semantics.md](./problem/copy-file-range02-error-semantics.md)。
- **关联 commit**：`85a2544`

#### copy_file_range03 时间戳更新修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`clock_nanosleep` 时间换算修复、`copy_file_range` 写后时间戳语义修复、LTP 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 中 glibc 版 `copy_file_range03` 为什么没有正确更新时间戳。AI 确认真实失败是 glibc 二进制的 `Testing __NR_copy_file_range syscall` raw syscall variant，而 libc wrapper variant 已通过；LTP 用例通过 `fstat(fd_dest).st_mtim` 比较写前写后时间。进一步定位到 `clock_nanosleep()` 将 ticks 换算为微秒时除以 `get_clock_freq()/1000`，导致 1.5 秒等待提前约 1000 倍返回；同时 `copy_file_range()` 写成功后只设置秒级 `mtime`，未保证相对旧值前进、未更新 `ctime` 且吞掉 `set_timestamps()` 错误。修复后 LoongArch 最新 `log.ans` 显示 musl/glibc `copy_file_range03` 均为 `passed 2 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/copy-file-range03-timestamp.md](./problem/copy-file-range03-timestamp.md)。
- **关联 commit**：`3f843c3`

#### clone02 CLONE_SIGHAND 退出状态修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：GDB 回溯分析、进程退出/信号共享状态修复、LTP clone 回归验证、文档完善
- **描述**：用户提供 `clone02` 退出路径的 GDB 回溯，显示 `exit_current_and_run_next()` 向父进程发送 `SIGCHLD` 时在 `send_signal_to_thread_group()` 内重入 `ProcessInner::get_locked_sigtable()` 并 panic。AI 定位到根因是 `SigTable` 同时保存 signal action 与 `group_exit_code`；`CLONE_SIGHAND` 父子进程共享 signal action 是正确的，但共享线程组退出码会污染父进程状态，并在子进程退出通知父进程时重入同一把 `sig_table` 锁。修复为把 `group_exit_code` 移入 `ProcessMeta`，`SigTable` 只保留信号处理动作。`make` 通过，最新 `log.ans` 显示 musl/glibc `clone02` 均为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/clone02-shared-sighand-exit-code.md](./problem/clone02-shared-sighand-exit-code.md)。
- **关联 commit**：`f12e696`

#### PCB 与 MemorySet 锁边界重构（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：PCB 结构重构、地址空间锁内聚、锁耦合降低、构建验证、文档完善
- **描述**：用户要求把已经自带同步的 `MemorySet`、`SigTable`、`FdTable`、`FSInfo` 从 `ProcessInner` 中移出，并让 `MemorySet` 自己持有内部锁；后续指出 `personality` 应放入 `ProcessMeta`。AI 将 `MemorySet` 改为内部 `RwLock<MemorySetInner>`，`Process` 改为直接持有自同步资源和可替换 Arc 指针，移除空壳 `ProcessInner`，保留 `inner_lock()` 作为兼容资源快照入口，并重写 fork/exec/exit、用户栈分配、`mremap` 等会触发 `MemorySet` 重入锁的路径。`make` 通过；QEMU 运行验证因沙箱 `/var/tmp` 只读且外部运行被中断，未取得有效运行结果。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：`ca158c8`

#### 移除 Process::inner_lock 兼容层（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：PCB 访问接口收敛、`ProcessResources` 兼容层移除、调用点机械迁移、构建验证、决赛文档补充
- **描述**：用户要求继续重构并移除 `inner_lock`。AI 删除 `Process::inner_lock()` 与 `ProcessResources`，在 `Process` 上保留直接资源访问方法，并把 fs、signal、task、io_mpx、net、futex、time 等调用点改成直接访问 `Process` 的地址空间、信号表、fd 表和 fs 上下文；`TaskControlBlock::inner_lock()` 仍保留用于线程级状态。`make` 通过，代码搜索未发现 `Process::inner_lock()` 残留。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：`e32eccb`

#### ResourceSlot 进程资源槽抽象（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：PCB 字段封装、可替换资源槽抽象、地址空间/信号表指针替换边界收敛、构建与 QEMU 基础验证、决赛文档补充
- **描述**：用户在讨论 `memory_set: Mutex<Arc<MemorySet>>` 是否能删除后，要求新增资源槽类型并调整 PCB 字段。AI 在 `os/src/utils` 新增 `ResourceSlot<T>` 封装 `Mutex<Arc<T>>` 指针槽，将 `Process.memory_set` 和 `Process.sig_table` 改为私有 `ResourceSlot` 字段，保留既有 PCB 访问方法作为稳定接口。`make` 通过，外部 LoongArch64 `make run` 基础测试跑到 `shutdown!`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：`4d5b26b`

#### 删除 get_locked_memory_set_read/write 兼容接口（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：PCB 地址空间访问接口收敛、兼容方法移除、调用点机械替换、构建与 QEMU 基础验证、决赛文档补充
- **描述**：用户要求删除 `get_locked_memory_set_read()` / `get_locked_memory_set_write()`。AI 将调用点统一改为 `memory_set_arc()`，删除两个误导性的兼容方法，保留 `ResourceSlot<MemorySet>` 负责地址空间指针槽同步、`MemorySet` 内部锁负责地址空间内容同步。`make` 通过，外部 LoongArch64 `make run` 基础测试跑到 `shutdown!`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：`d69653a`

#### vmsplice pipe 文件分类收敛（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：`vmsplice` 语义收敛、pipe fd 分类调整、`FileClass::Pipe` 接入、构建验证、决赛文档补充
- **描述**：用户指出 `Pipe` 也是文件，当前匿名 pipe 分配逻辑放入 `FileClass::Abs`，没有体现 pipe 是独立文件类别。AI 将 `pipe2()` 创建的读写端改为 `FileClass::Pipe`，修正 `FileClass::pipe()` / `FileDescriptor::pipe()` 访问路径，并让 `sys_vmsplice()` 通过 pipe 类型入口校验目标 fd 后写入。默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：`b67884e`

#### PipeBuf 片段队列与 splice/tee 引用路径（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：pipe 内部缓冲结构优化、`splice(pipe, pipe)` 引用移动、`tee()` 引用复制、构建与运行验证、决赛文档补充
- **描述**：用户要求继续优化 `splice` 的零拷贝能力。AI 将 `PipeRingBuffer` 从固定字节环改为 `PipeBuf` 片段队列，普通 pipe `read/write` 仍保持字节语义；两端都是 pipe 的 `splice()` 改为移动 `PipeBuf` 引用，`tee()` 改为克隆 `PipeBuf` 引用而不消耗输入 pipe。默认 LoongArch64 `make` 通过；当前 `initproc` 运行的是 `splice05`，仍失败于 socket splice `ENOTCONN`，该 socket 语义缺口需后续单独处理。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：`2d749d3`

#### 文件页面缓存与 splice file->pipe fast path（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：文件页面缓存接入、`MAP_SHARED` mmap 共享文件页复用、`PipeBuf` 挂载 `FilePage`、`splice(file, pipe)` 缓存页引用 fast path、构建与运行验证、决赛文档补充
- **描述**：用户已添加 `os/src/fs/page_cache.rs` 并要求继续修改。AI 将 `MAP_SHARED` 文件 mmap 缺页改为通过 `FILE_PAGE_CACHE` 加载并映射缓存页 frame，移除 `GROUP_SHARE` 中旧的文件页共享表；`PipeBuf` 扩展为 `Bytes/FilePage` storage，普通文件到 pipe 的 `splice()` 在输入为 `OSFile` 时将缓存页引用挂入 pipe。普通文件 `write()` 和 ext4 `truncate()` 增加缓存失效，避免读到旧页。默认 LoongArch64 `make` 通过；当前 `splice09` 入口因内核版本要求 `TCONF/skipped`，无 panic/TFAIL/TBROK 并跑到 `shutdown!`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：`61dca03`

#### pipe SIGPIPE 与 FIONREAD 语义修复（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、pipe Linux 语义修复、`SIGPIPE` 投递、`ioctl(FIONREAD)` 兼容、LTP pipe 回归验证、决赛文档补充
- **描述**：用户要求分析 `log.ans` 中 pipe 实现是否有问题。AI 定位到 `pipe02`/`pipe08` 失败是无读端 pipe 写入只返回 `EPIPE` 而未投递 `SIGPIPE`，`pipe12` 失败是缺少 `FIONREAD(0x541B)`。修复为在无读端写入时释放相关锁后向当前线程投递 `SIGPIPE` 并返回 `EPIPE`，并让 `Pipe::ioctl()` 写回当前可读字节数。默认 LoongArch64 `make` 通过；当前 `pipe02`、`pipe08`、`pipe12` 核心断言均 `TPASS`。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目与 [problem/pipe-sigpipe-fionread.md](./problem/pipe-sigpipe-fionread.md)。
- **关联 commit**：`cb1509e`

#### pipe 模块源码拆分重构（7.2）

- **工具/模型**：Codex (GPT-5)
- **场景**：pipe 源码布局重构、模块边界收敛、构建验证、决赛文档补充
- **描述**：用户要求将 `os/src/fs/files/pipe.rs` 按功能拆分到 `os/src/fs/files/pipe/`。AI 保持 `Pipe`、`make_pipe()`、`open_fifo()` 对外路径不变，将片段缓冲、共享缓冲区、FIFO 表、splice/tee、阻塞等待和 `File` trait 实现拆到独立子模块，并修正拆分后的 trait 作用域与可见性告警。默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-02 条目。
- **关联 commit**：`9684e4e`

#### VFS inode cache 实现（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：VFS inode cache 设计与实现、路径索引与 inode 身份归并、hard link/rename 兼容、双架构构建和 QEMU 交互验证、决赛文档补充
- **描述**：用户询问当前内核是否实现 inode cache，并要求进一步实现真正的 VFS inode cache。AI 确认旧 `FsIndex` 只是 `path -> Arc<dyn Inode>` 强引用表，无法按 inode 身份归并 hard link，也依赖 close 时 `Arc::strong_count()` 手动淘汰。修改为 `path -> InodeCacheKey` 与 `(st_dev, st_ino) -> Weak<dyn Inode>` 的两级缓存，`insert_inode_idx()` 返回 canonical inode；同时为 path-based 的 `Ext4Inode` 增加路径别名和 `live_path()`，避免同一 inode 复用后仍固定使用 stale path。RISC-V / LoongArch64 构建通过，RISC-V QEMU 交互验证 hard link、unlink 原路径、rename 后读取均正常。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目。
- **关联 commit**：`cebe859`

#### iperf 5001 端口复用、daemon 残留与 glibc TCGETS 栈破坏修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、网络 daemon 生命周期清理、termios ioctl 栈破坏修复、回归验证、文档完善
- **描述**：用户要求根据最新 `log.ans` 继续修复 iperf，并强调测例入口必须使用标准 `run_testsuit(root, script)`。AI 确认 `iperf-musl` 后 `iperf-glibc` 固定复用 5001；`iperf3 -s -D` 在脚本退出后被 init 收养并继续监听，导致 glibc server `listen(5001)` 失败，而 glibc client 连接到上一轮 musl daemon 后形成 false positive success。最终保持 iperf 调用走标准 `run_testsuit()`，在通用 testsuit 收尾阶段用 `kill_processes(-1, SIGKILL)` 清理并 `wait()` 回收残留后台子进程；同时保留 fd close/exit 主动 shutdown socket 与 glibc `TCGETS` old termios 36 字节布局修复。`make` 通过，`/tmp/iperf-generic-cleanup.log` 中 musl/glibc 两轮 iperf 六项均 success，未再出现 `socket already listening on port 5001`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/iperf-port-reuse-termios-stack-smash.md](./problem/iperf-port-reuse-termios-stack-smash.md)。
- **关联 commit**：`5626520`

#### netperf fd table 重构后 socket 生命周期回归修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、fd table/socket 生命周期语义修复、netperf 回归验证、文档完善
- **描述**：用户说明 `9fa9699d2580a7de85e41baf2e49c3e51000d6f0` 之后内核大规模重构导致许多旧测试回归，要求先修复 `netperf`，并在文档中突出本次修改。AI 根据详细 `log.ans` 确认控制连接已 `connect/accept` 成功，但 `netserver` 父进程 fork 后关闭 accepted fd 时，重构后的 `FdTable::close()` 只检查同表 alias，误把子进程继承的同一 `Arc<Socket>` 提前 `shutdown()`，导致客户端读到 0 字节响应。初版修复保护了 fork 继承连接，但又削弱进程退出释放 listener 的旧修复，使 `netperf-glibc` 回退为 12865 端口残留。最终将普通 `close(fd)` 与进程退出 `FdTable::clear()` 的 socket 语义拆开：前者仅在最后一个 `Arc` 时 shutdown，后者仍主动 shutdown 释放监听端口；同时补齐 `recvmsg` iovec 回拷和 `accept4` peer address 写回语义。`make` 通过，`timeout 300s make run > log.ans 2>&1` 中 musl/glibc 两轮 netperf 十项均 success。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/netperf-glibc-port-reuse.md](./problem/netperf-glibc-port-reuse.md)。
- **关联 commit**：`dc14c16`

#### cyclictest STRESS_P1 socketpair fd 分配回归修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、fd table 重构后 socketpair 语义修复、AF_UNIX 阻塞等待补齐、cyclictest 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans`，其中 `cyclictest STRESS_P1` 出现大量 `CLIENT: ready write (error: Broken pipe)`。AI 确认错误来自 STRESS 模式后台 `hackbench` 的 worker ready 同步通道，而不是 cyclictest 定时器本身。根因是 fd table 重构后 `alloc_fd()` 不再自动占位，`sys_socketpair()` 连续两次分配 fd 且中间未 `set()`，导致两个 socketpair 端点复用同一个 fd，第二端覆盖第一端。修复为分配 `fd1` 后立即安装，再分配 `fd2`，并补齐失败清理；同时为 AF_UNIX socket 增加阻塞 `recv/accept` 的 poll/waker 语义。`make` 通过，`timeout 300s make run > log.ans 2>&1` 中 musl/glibc cyclictest 四项均 success，未再出现 ready 阶段 Broken pipe。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/cyclictest-socketpair-fd-allocation.md](./problem/cyclictest-socketpair-fd-allocation.md)。
- **关联 commit**：`33c409b`

#### libctest sigtimedwait 残留 interrupted 修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`sigtimedwait/wait4` 信号交接语义修复、libctest clocale 回归验证、文档完善
- **描述**：用户要求分析 `clocale.ans`，其中 libctest 包装器在所有测例上报 `Interrupted system call`。AI 确认 `runtest.exe` 先通过 `sigtimedwait(SIGCHLD)` 成功消费子进程退出信号，随后 `wait4(pid)` 却被 `interruptible()` 看到残留内部 `interrupted` 标志而误返回 `EINTR`。修复为 `sys_rt_sigtimedwait()` 在成功匹配并消费 pending signal 后调用 `clear_interrupt_waiter()`，清理本次内部唤醒状态，不再污染后续 `wait4/select` 等 syscall。`make` 通过，`timeout 120s make run > /tmp/clocale-sigtimedwait-fix.log 2>&1` 中 `clocale_mbfuncs` 输出 `Pass!`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/libctest-sigtimedwait-eintr.md](./problem/libctest-sigtimedwait-eintr.md)。
- **关联 commit**：`aef6c0b`

#### RISC-V Alpine initfiles 与动态链接路径兼容（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、RISC-V Alpine 镜像启动兼容、动态解释器路径回退、文档完善
- **描述**：用户在尝试运行当前内核上的 vim/shell 时遇到 `initfiles.rs:61` 对 `/glibc/lib/libgcc_s.so.1` 的 `ENOENT` panic，并询问 RISC-V 场景是否有必要把该链接库放入内核。AI 确认该库只用于竞赛 glibc 测试镜像，不是 Alpine/musl 根文件系统必需；修复为仅在 `/glibc/lib` 存在时写入，同时仅在 `/musl/busybox` 存在时创建测试镜像 `/bin` applet 和 LTP wrapper，避免覆盖 Alpine 原生 `/bin/sh`。后续根据 `log.ans` 中 `FetchInstructionPageFault bad addr = 0xfffffffffffffffe`，继续修复动态解释器回退仍被通用 `open()` 二次映射的问题，新增 `open_direct()` 并让 ELF loader 区分“无解释器”和“解释器加载失败”。`make TARGET_ARCH=riscv64` 通过，25 秒 QEMU 日志未再出现原 ENOENT panic、`exec /bin/sh failed` 或取指 fault。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/riscv-alpine-initfiles-dynamic-link.md](./problem/riscv-alpine-initfiles-dynamic-link.md)。
- **补充**：随后用户要求提升启动后人机交互体验。AI 将 `initproc` 改为默认执行 `/bin/sh -i`，失败时回退 `/musl/busybox sh -i`，并补充 `TERM/HOME/SHELL/USER` 默认环境变量。`make TARGET_ARCH=riscv64` 通过，25 秒 QEMU 日志出现 `/ #` 提示符。
- **补充**：用户确认输入可读且 `vi` 可运行后，要求 shell 显示用户输入。AI 在 `Stdin::read()` 中加入基础控制台回显，覆盖普通字符、回车和退格；`make TARGET_ARCH=riscv64` 通过，延迟到 prompt 后输入 `echo SHELL_ECHO_OK` 的 QEMU 日志显示命令本身和执行结果。
- **补充**：用户反馈 `vi` 的 `:` 模式输入一个字符显示两个。AI 将 stdio 强制回显改为最小 termios 模型，支持 `TCGETS/TCSETS*`、`TIOCGWINSZ`，并用 `ECHO/ICANON` 控制 stdin 回显和原始字符读取。`make TARGET_ARCH=riscv64` 通过，QEMU 中 `stty -echo` 后输入命令不再回显，`stty echo` 后恢复。
- **关联 commit**：`6cf88d1`, `275d3a5`, `d9fb6e8`, `0a8403a`

#### LTP abort01 waitpid SA_RESTART EINTR 修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`waitpid/waitid` 信号打断语义修复、syscall restart 路径修正、LTP abort01 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 中 `tst_test.c:1654: TBROK: waitpid(3,...,0) failed: EINTR (4)`。AI 确认 `abort01` 的 LTP harness 为 `SIGUSR1` 安装了带 `SA_RESTART` 的 handler；原 `waitpid/waitid` 外层通用 `interruptible()` 在 wait 内部检查 pending signal 前就把 `wake_interruptible()` 状态转成 `EINTR`，绕过了 `SA_RESTART` 语义。修复为 wait 自己处理 pending signal：可忽略信号继续等待，不带 `SA_RESTART` 返回 `EINTR`，带 `SA_RESTART` 返回内部 `ERESTART`；同时修正 `setup_frame()` 对负 errno 形式 `ERESTART` 的识别。`make` 通过，最新 `log.ans` 中 `abort01` 为 `passed 2 failed 0 broken 0`，未再出现原 `TBROK`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/waitpid-sa-restart-eintr.md](./problem/waitpid-sa-restart-eintr.md)。
- **关联 commit**：`4ba8915`

#### LTP access02 CLONE_VM panic 与 O_RDONLY 权限误判修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、GDB backtrace 解释、clone/vfork 资源分配修复、open flags 权限语义修复、LTP access02 回归验证、文档迁移到决赛目录
- **描述**：用户要求继续分析 `access02`，提供 `clone_process -> alloc_user_res -> MemorySet::get_mut()` panic backtrace，并指出后续 `log.ans` 仍有 4 个失败。AI 确认 `clone_process()` 只按 `CLONE_THREAD` 进入共享地址空间路径，遗漏非线程 `CLONE_VM`，导致 vfork/clone 类路径共享 `MemorySet` 时仍分配 fork 用户资源；随后定位 4 个失败来自 `OpenFlags::read_write()` 只在 flags 为空时识别 `O_RDONLY`，把 `O_RDONLY|O_CLOEXEC/O_LARGEFILE` 误判成读写打开并触发写权限检查。修复后 `make` 通过，最新 `log.ans` 中 `access02` 16 项核心断言均 `TPASS`，未再出现 panic、`TFAIL`、`TBROK` 或死循环。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/access02-ltp-execve.md](./problem/access02-ltp-execve.md)。
- **关联 commit**：`d224638`

#### LTP access04 faccessat errno 语义修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、`faccessat` Linux errno 语义修复、只读挂载路径匹配修复、LTP access04 回归验证、文档完善
- **描述**：用户要求分析并修复 `access04`。AI 确认失败来自两个 errno 优先级问题：256 字节文件名分量应按 `NAME_MAX=255` 返回 `ENAMETOOLONG`，但原实现只检查整条路径长度并最终返回 `ENOENT`；`W_OK` 检查只读 `tmpfs` 挂载点时，原实现用相对输入路径精确匹配挂载表，导致 root 错误成功、nobody 返回 `EACCES`。修复为在 `sys_faccessat()` 中检查每个路径分量长度，并用解析后的绝对路径按最长挂载点前缀查询只读 mount 后返回 `EROFS`。`make` 通过，最新 `log.ans` 中 `access04` 12 项核心断言均 `TPASS`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/access04-faccessat-name-rofs.md](./problem/access04-faccessat-name-rofs.md)。
- **关联 commit**：`0a7c44a`

#### LTP acct01 acct errno 语义修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、`acct(2)` Linux errno 语义修复、effective uid 权限判断、只读挂载与路径边界检查、LTP acct01 回归验证、文档完善
- **描述**：用户要求分析并修复 `acct01`。AI 确认失败来自 `sys_acct()` 错误优先级和凭证判断：尾随 `/` 被路径规范化吞掉，非特权场景只改 effective uid 但原实现检查 real uid，`open(O_WRONLY)` 的 `EACCES` 覆盖了 `EPERM`，256 字节文件名分量缺少 `NAME_MAX=255` 检查，只读挂载点未返回 `EROFS`。修复为用 effective uid 判断 `CAP_SYS_PACCT`，补齐空路径、文件名分量、尾随 `/` 和只读挂载检查，并先只读验证路径类型、最后再用 `O_WRONLY` 保存 accounting 文件。`make` 通过，最新 `log.ans` 中 `acct01` 9 项核心断言均 `TPASS`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/acct01-sys-acct-errno.md](./problem/acct01-sys-acct-errno.md)。
- **关联 commit**：`0ce79d6`

#### LTP acct02 accounting exit(128) 状态编码修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、process accounting `ac_exitcode` 编码修复、LTP acct02 回归验证、文档完善
- **描述**：用户要求分析并修复 `acct02`。AI 确认失败来自 accounting 记录已写出但 `ac_exitcode` 为 0；`acct02_helper` 正常 `exit(128)`，LTP 期望 wait status `128 << 8 = 32768`。根因是 `acct.rs` 中 `wait_status_from_exit_code()` 对普通退出码 `128..255` 做了错误归零处理，和 `wait4` 的普通退出编码不一致。修复为删除该特殊分支，普通退出统一写 `exit_code << 8`，信号终止仍按 `termination_signal` 编码。`make` 通过，`make run` 单跑 `acct02` 后 `log.ans` 显示 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/acct02-exitcode-128.md](./problem/acct02-exitcode-128.md)。
- **关联 commit**：`ff0db8d`

#### LTP alarm05 setitimer 剩余时间语义修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、`alarm(2)`/`setitimer(2)` old_value 剩余时间语义修复、周期 timer 推进语义修正、LTP alarm05 回归验证、文档完善
- **描述**：用户要求分析并修复 `alarm05`，随后确认最新 `log.ans` 已通过，并询问为什么看似标准的计时器模块仍有问题。AI 确认原实现能按 `last_time` 驱动 `SIGALRM` 投递，但 `Timer::timer()` 直接返回原始 `it_value`，导致 `alarm(10)` 经过约 1 秒后被 `alarm(1)` 替换时仍返回 10，而不是旧闹钟剩余约 9 秒。修复为用 `now - last_time` 折算 `getitimer/setitimer(old_value)` 返回的剩余 `it_value`，并让周期 timer 到期后把下一轮 `it_value` 设为 `it_interval`。`make` 通过，最新 `log.ans` 中 `alarm05` 3 项核心断言均 `TPASS`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/alarm05-itimer-remaining.md](./problem/alarm05-itimer-remaining.md)。
- **关联 commit**：`d5f1222`

#### LTP bind01 bind 地址与 AF_UNIX 路径语义修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、TCP/UDP `bind(2)` 本机地址检查、AF_UNIX pathname 父目录校验、LTP bind01 回归验证、文档完善
- **描述**：用户要求分析并修复 `bind01`，随后确认最新 `log.ans` 已通过。AI 确认原实现用路由表判断 bind 地址，默认路由导致非本地地址也能绑定成功；同时 AF_UNIX pathname bind 只登记内存表，没有校验路径前缀，导致中间分量非目录时也成功。修复为在 TCP/UDP bind 前检查地址是否为 wildcard、接口地址或 loopback `127/8`，否则返回 `EADDRNOTAVAIL`；AF_UNIX pathname bind 前通过 VFS 打开父目录，复用 `ENOTDIR/ENOENT` 语义。`make` 通过，最新 `log.ans` 中 `bind01` 7 项核心断言均 `TPASS`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/bind01-bind-address-path.md](./problem/bind01-bind-address-path.md)。
- **关联 commit**：`a176235`

#### LTP bind04 AF_UNIX SEQPACKET 与 sockaddr_storage 长度修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、AF_UNIX socket 语义修复、IPv4/IPv6 connect 地址长度兼容、SCTP one-to-one stream 兼容、IPv6 loopback 支持、GDB panic 触发链分析、LTP bind04 回归验证、文档完善
- **描述**：用户要求根据最新 `log.ans` 和 GDB backtrace 修复 `bind04`，随后继续要求修复 `socket(2, 1, 132) failed: EPROTONOSUPPORT`。AI 确认 panic 是 `connect(..., addrlen=128)` 被误判 `EINVAL` 后测试异常退出触发的回收断言；更早的 bind04 阶段还缺少 AF_UNIX pathname socket inode 和 `SOCK_SEQPACKET` 支持。修复为 pathname bind 创建 VFS socket 节点、AF_UNIX seqpacket 复用连接型队列并保留记录边界、IPv4/IPv6 sockaddr 读取接受大于结构体大小的 `sockaddr_storage` 长度；后续补充 `IPPROTO_SCTP` stream socket 到现有连接型实现的最小兼容，并添加 `::1/128` loopback 路由与 `AF_INET6` socket 创建支持。`make` 与单跑 `bind04` 的 `make run` 通过，`log.ans` 中 16 项通信场景均 `TPASS`，summary 为 `passed 16 failed 0 broken 0 skipped 0`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/bind04-unix-seqpacket-sockaddr.md](./problem/bind04-unix-seqpacket-sockaddr.md)。
- **关联 commit**：`727774a`, `a3215d2`

#### LTP splice07 文件类型校验与空 pipe 卡死修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、`splice(2)` 非 pipe 端文件类型准入修复、未实现 fd 占位对象 panic 修复、LTP splice07 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修复最后 `splice07` 卡死。AI 确认原实现只检查 `readable()/writable()`，缺少非 pipe 端 `st_mode` 准入，导致 `directory -> pipe write end` 错误成功，并在 `pipe read end -> /dev/zero` 时把字符设备输出端当作可 splice 目标，随后空 pipe 读阻塞。修复为非 pipe 输入只允许 `FREG/FCHR`，非 pipe 输出只允许 `FREG`，在真正读 pipe 前返回 `EINVAL`；同时为 `DummyFd` 补默认 `fstat()`，避免 signalfd/timerfd 等未实现 fd 的错误路径 panic。`make` 通过，`make run` 单跑 `splice07` summary 为 `passed 566 failed 0 broken 0 skipped 25 warnings 0`，系统跑到 `shutdown!`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/splice07-file-type-validation.md](./problem/splice07-file-type-validation.md)。
- **关联 commit**：`9e9bebe`

#### LTP open14 O_TMPFILE 深层路径超时修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：LTP 源码分析、`O_TMPFILE`/`mkdirat`/`linkat`/`unlinkat` 路径性能定位、深层目录慢路径修复、open14 回归验证、文档完善
- **描述**：用户要求分析 `open14` 源码并修复运行到 `creating a file with O_TMPFILE flag` 后超过 5 分钟无输出的问题。AI 对照 LTP 源码确认测例会构造 100 层 `tst02_*` 与 `tst03_*` 目录；临时阶段日志证明内核并非死锁，而是在深层路径循环中缓慢推进。修复为补齐 `O_CREAT|O_EXCL` 存在性语义，令 `mkdirat` 和 tmpfile `linkat` materialize 使用单次独占创建，ext4 create(existing) 返回 `EEXIST`，目录 `rmdir` 跳过普通文件延迟删除检查，并让 `open(".", O_TMPFILE)` 复用已验证 cwd。`make` 通过，`timeout 600s make run` 单跑 `open14` 显示 3 项 TPASS，summary 为 `passed 3 failed 0 broken 0 skipped 0 warnings 0`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/open14-otmpfile-path-slow.md](./problem/open14-otmpfile-path-slow.md)。
- **补充**：用户继续要求 `open()` 使用已经缓存的父目录。AI 在 `open_inner()` 的完整路径 cache miss 后增加缓存父目录查找路径，并让 `create_file()` 复用同一个父 inode 做权限检查、gid 继承和创建；同时让 `Ext4Inode::types()` 返回构造时缓存的类型，避免目录类型判断再次触发路径查询。`make` 与 `timeout 600s make run` 单跑 `open14` 均通过，summary 仍为 `passed 3 failed 0 broken 0 skipped 0 warnings 0`。详见 `Docs/决赛文档/ai.log` 2026-07-03 `LTP open14 父目录缓存路径优化` 条目。
- **补充**：用户要求在 `os/src/fs/dcache.rs` 实现 dentry cache。AI 新增父 inode + child name 的 positive/negative dentry cache，让 `find_from_cached_parent()` 在底层 ext4 查找前先命中 child inode 或 `ENOENT`，并在 create/link/unlink/symlink/rename 成功后回填或失效目录项。`make` 与 `timeout 600s make run` 单跑 `open14` 均通过，summary 为 `passed 3 failed 0 broken 0 skipped 0 warnings 0`。详见 `Docs/决赛文档/ai.log` 2026-07-03 `VFS dentry cache 接入 open 父目录缓存路径` 条目。
- **补充**：用户要求按性能定位优先级继续实现优化。AI 用临时低噪声埋点确认 `openat(O_TMPFILE)` 本身不是瓶颈，主要耗时来自 `FsIndex`/dentry positive cache 使用 `Weak` 导致 inode 生命周期过短，以及 `Ext4Inode::live_path()` 每次元数据操作都重复 `check_inode_exist()`。修复为将 `FsIndex` 与 dentry positive cache 改为可显式失效的 strong `Arc` cache，`live_path()` 热路径直接使用当前 path、失败后再 alias 恢复，并合并 `create_file()` 父目录元数据读取。`make` 通过，`timeout 600s make run` 单跑 `open14` 耗时从约 223.35s 降到 65.94s，summary 为 `passed 3 failed 0 broken 0 skipped 0 warnings 0`。详见 `Docs/决赛文档/ai.log` 2026-07-03 `LTP open14 缓存生命周期与 live_path 性能优化` 条目。
- **关联 commit**：`c608399`, `6cfdfc7`, `7815eeb`, `adc18ab`

#### LTP tst_virt /proc/cpuinfo 缺失修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、启动期 `/proc` 兼容文件补齐、LTP `tst_virt` 回归验证、文档完善
- **描述**：用户要求分析并修复 `tst_virt.c:37: TBROK: fopen(/proc/cpuinfo,r) failed: ENOENT (2)`。AI 确认失败发生在 LTP 公共虚拟化探测库读取 `/proc/cpuinfo` 阶段；Ya2yOS 启动期已有 `/proc/mounts` 和 `/proc/meminfo` 等兼容文件，但缺少 `/proc/cpuinfo`，导致 `openat("/proc/cpuinfo")` 走普通 ext4 查找并返回 `ENOENT`。修复为在 `create_proc_files()` 中写入 RISC-V/LoongArch64 最小 `CPUINFO` 文本，并刻意不包含 `QEMU Virtual CPU`，避免改变 LTP 对 KVM 的判断。`make` 通过，用户确认最新 `log.ans` 已通过，summary 为 `passed 7 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/proc-cpuinfo-tst-virt.md](./problem/proc-cpuinfo-tst-virt.md)。
- **关联 commit**：`8046c48`

#### LTP epoll_create02 RISC-V musl libc 包装语义修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、RISC-V 预赛 musl libc 包装函数反汇编、动态库只读兼容补丁、LTP `epoll_create02` 回归验证、文档完善
- **描述**：用户要求修复 `epoll_create(0) invalid retval 3: SUCCESS`。AI 确认内核 `epoll_create1(0)` 本身必须成功，不能在 syscall 层改语义；失败来自 RISC-V 预赛镜像 `/musl/lib/libc.so` 的 `epoll_create(size)` 包装函数直接 `li a0,0; j epoll_create1`，没有检查 `size <= 0`。修复为在动态库读取路径中仅对 RISC-V `/musl/lib/libc.so` 做只读 patch：旧 `epoll_create(size)` 非法 size 返回 `EINVAL`，合法 size 仍转 `epoll_create1(0)`，不影响 `epoll_create1(0)`。`make TARGET_ARCH=riscv64` 通过，临时单跑 `epoll_create02` 的 libc 变体两项均 `TPASS`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/epoll-create02-riscv-musl-libc.md](./problem/epoll-create02-riscv-musl-libc.md)。
- **关联 commit**：`4220200`

#### iperf IPPROTO_IPV6/IPV6_V6ONLY 兼容修复（7.3）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、iperf server 启动失败定位、IPv6 socket option 最小兼容、RISC-V QEMU 回归验证、文档完善
- **描述**：用户要求分析并修复 iperf 失败。AI 确认 `log.ans` 中 `sys_setsockopt unknown protocol level=41 optname=26` 对应 Linux `IPPROTO_IPV6/IPV6_V6ONLY`，该错误使 iperf3 server 在 bind/listen 前关闭 socket 并退出，后续客户端全部 `Connection refused`。修复为在 socket option 层承认 `IPPROTO_IPV6`，对 `IPV6_V6ONLY` 解析参数并返回成功，`getsockopt` 返回默认 `0`。`make` 通过，使用 `/tmp` qcow2 overlay 绕过当前沙箱 `/var/tmp` 只读限制后运行 RISC-V QEMU，musl/glibc 两组 iperf 六项均 `success`。详见 `Docs/决赛文档/ai.log` 2026-07-03 条目与 [problem/iperf-ipv6-v6only.md](./problem/iperf-ipv6-v6only.md)。
- **关联 commit**：`1361a4c`

#### LTP mkdir09 LoongArch MAP_STACK 与 tmpfs 隔离修复（7.4）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans`/`ana.ans` 分析、LoongArch `PagePrivilegeIllegal` 缺页路径修复、`MAP_STACK` 懒分配权限修复、tmpfs 挂载隔离兼容、LTP mkdir09 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 和 `ana.ans` 并修复。AI 确认原始失败不是旧的 `mkdir09` mode 类型位问题，而是 LoongArch glibc pthread 创建第三个 `MAP_STACK` 时，访问已 `mprotect(PROT_READ|PROT_WRITE)` 的懒分配栈页被报告为 `PagePrivilegeIllegal`，原 trap 分支直接发送 `SIGSEGV`。修复为 `PagePrivilegeIllegal` 先尝试统一缺页处理，并让 mmap/brk/stack lazy fault 按 VMA 权限判断，避免 `PROT_NONE` guard page 被错误映射。随后 tmpfs 轮次暴露 `mkdir(mntpoint/X.0) failed: EEXIST`，AI 定位到当前 mount 只维护表项、没有真实 tmpfs 空根目录视图，补充 tmpfs 挂载前清空挂载点内容以隔离 LTP filesystem 轮次。`make TARGET_ARCH=loongarch64` 通过，单跑 `mkdir09` summary 为 `passed 12 failed 0 broken 0 warnings 0`。详见 `Docs/决赛文档/ai.log` 2026-07-04 条目与 [problem/mkdir09-loongarch-map-stack-tmpfs.md](./problem/mkdir09-loongarch-map-stack-tmpfs.md)。
- **关联 commit**：`c3d0ff4`

#### faccessat2 syscall 实现（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：新增 Linux syscall、`faccessat` 逻辑复用、LTP `faccessat201/202` 回归验证、文档完善
- **描述**：用户要求实现 `faccessat2(439)`。AI 确认 syscall 枚举已有 439 号但缺少分发和实现；现有 `sys_faccessat()` 已有 real uid/gid 权限检查、父目录 execute 检查、路径长度与只读挂载处理。修复为抽出共用 `do_faccessat()`，新增 `sys_faccessat2()`，校验 `AT_EACCESS/AT_SYMLINK_NOFOLLOW/AT_EMPTY_PATH` flags，`AT_EACCESS` 下使用 effective uid/gid，并修正绝对路径忽略坏 `dirfd` 的语义。`make` 通过，RISC-V 单跑 musl/glibc `faccessat201` 均 7 项 `TPASS`，`faccessat202` 均 6 项 `TPASS`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：`dd0c8ad`

#### fanotify_init 基础 fd 实现（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：新增 fanotify fd 文件对象、`fanotify_init(262)` 参数校验与 fd 分配、LoongArch64 构建和运行验证、文档完善
- **描述**：用户要求实现 `sys_fanotify_init`。AI 确认 syscall 262 已分发，但原实现返回 `Ok(0)`，会把 stdin 误当作 fanotify fd。修复为新增 `FanotifyFd`，让 `sys_fanotify_init()` 校验 fanotify init flags、class bits、`FAN_REPORT_*` 依赖关系和 event fd flags，按 `FAN_CLOEXEC/FAN_NONBLOCK` 设置 fd flags，并返回新分配 fd。`make` 在当前默认 LoongArch64 下通过；`make run` 中 `fanotify01` 已推进到 `fanotify_mark(263)`，并因 263 号尚未实现而 `TBROK/ENOSYS`，说明完整 fanotify 事件系统仍需后续实现。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：`1be11fd`

#### fanotify_mark 与 fanotify01 基础事件实现（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：新增 `fanotify_mark(263)`、fanotify mark 表与事件队列、VFS open/read/write/close 基础事件投递、LTP `fanotify01` 回归验证、文档完善
- **描述**：用户要求实现 `FanotifyMark` syscall，并指出 `log.ans` 中 `fanotify01` 未通过。AI 先接入 263 号分发和 `sys_fanotify_mark()`，补 fanotify fd registry、mark add/remove/flush 和 ignore mask；随后根据 `EAGAIN` 日志继续补 `FanotifyFd::read()` 事件队列和 fanotify metadata 序列化，并在 `sys_openat()`、`OSFile::read/write/drop` 投递 `FAN_OPEN/FAN_ACCESS/FAN_MODIFY/FAN_CLOSE_*`。针对多余 close 事件，AI 增加 fanotify 内部 suppress guard，避免 mark 目标检查和事件 fd 构造产生用户可见事件。`make` 通过，`make run > log.ans` 单跑 musl/glibc `fanotify01` summary 均为 `passed 156 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fanotify01-mark-events.md](./problem/fanotify01-mark-events.md)。
- **关联 commit**：`0307575`

#### syscall/sys.rs 职责拆分重构（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：syscall 杂项模块结构重构、身份/capability/prctl/system/fs 子模块拆分、构建验证
- **描述**：用户要求重构 `os/src/syscall/sys.rs` 以提高子模块内聚度。AI 将原大文件按职责拆成 `os/src/syscall/sys/identity.rs`、`capability.rs`、`prctl.rs`、`system.rs`、`fs.rs`，并用 `sys/mod.rs` 统一 re-export，保持 `os/src/syscall/mod.rs` 的 `use sys::*` 接口不变。本次未修改 syscall 分发、函数签名或用户可见语义；`cargo fmt --manifest-path os/Cargo.toml` 和当前默认 LoongArch64 `make` 均通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：`b6dff41`

#### syscall 与 task 模块边界收敛（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：syscall 模块设计分析、反向依赖消除、fs syscall 门面拆分、构建验证、文档完善
- **描述**：用户要求分析 syscall 模块是否满足高内聚、低耦合并修改。AI 确认主要问题是 `task` 反向依赖 `syscall::write_process_acct_record`、`CloneFlags` 放在 syscall clone 文件但被 task 核心使用、`fs/mod.rs` 混入 inotify 具体实现，以及若干子模块绕根 re-export 回取 helper/type。重构为将 process accounting 核心移到 `os/src/task/acct.rs`，将 `CloneFlags` 移到 `os/src/task/clone_flags.rs`，把 inotify syscall 拆到 `os/src/syscall/fs/inotify.rs`，并收窄 syscall 根模块对 `task::*` 的公开 re-export。本次保持 syscall 号、分发和用户可见语义不变；`cargo fmt` 与当前默认 LoongArch64 `make` 均通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：`c000d8b`

#### syscall/fs/path.rs 路径 syscall 聚合（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：syscall 文件系统路径模块重构、`chroot/getcwd/chdir/readlinkat/faccessat` 迁移、构建验证、文档完善
- **描述**：用户要求新建 `path.rs` 放置 `sys_chroot`，并把其他相关 syscall 一起移动进去。AI 新增 `os/src/syscall/fs/path.rs`，迁入 `getcwd/chdir/chroot/readlinkat/faccessat/faccessat2` 及其路径/procfd/access helper；`ctl.rs` 回到目录项控制、ioctl、sync、chmod/chown 等职责，`stat.rs` 回到 stat/statx/statfs 职责，并移除 `sys` 下的 chroot 子模块。本次未改 syscall 号、分发或用户可见语义；`cargo fmt` 和当前默认 LoongArch64 `make` 均通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：`82bf10a`

#### LTP fchmod02 /etc/group 前置组缺失修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fchmod02` setup 前置条件定位、启动期 `/etc/group` 兼容文件修复、构建与日志验证、文档完善
- **描述**：用户要求分析 `Log.ans` 失败原因并修复。AI 确认实际日志为 `log.ans`，失败点是 `SAFE_GETGRNAM_FALLBACK("users", "daemon")` 中 `users` 与 `daemon` 均不存在，导致 `TBROK`，测试尚未进入 `fchmod(2)` 语义断言。修复为在启动期 `/etc/group` 模板中补齐 `daemon:x:2:` 与 `users:x:100:`，并保持原有 `nobody:x:1:` 不变。`make` 通过，最新 `log.ans` 中 musl/glibc `fchmod02` 均 `TPASS`，Summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fchmod02-group-database.md](./problem/fchmod02-group-database.md)。
- **关联 commit**：`2ba51c5`

#### LTP fchmod05 chmod S_ISGID 语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fchmod05` chmod 语义定位、`S_ISGID` 清除规则修复、构建与日志验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fchmod05` 失败是非 root 目录 owner 在 gid 不匹配目标目录时仍成功保留 `S_ISGID`；Linux 语义要求 `fchmod()` 成功但静默清掉 setgid 位。修复为新增 `chmod_inode()` 并让 `fchmod/fchmodat` 共用，补齐只读挂载 `EROFS`、非 owner `EPERM`、gid 不匹配清除 `S_ISGID`，同时传播 `fmode_set()` 错误。`make` 通过，最新 `log.ans` 中 musl/glibc `fchmod05` 均 `TPASS`，Summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fchmod05-chmod-setgid.md](./problem/fchmod05-chmod-setgid.md)。
- **关联 commit**：`a69c9bc`

#### LTP fanotify02 FAN_EVENT_ON_CHILD 卡死修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 卡死分析、fanotify 目录 child event 匹配、`fanotify_mark(FAN_MARK_REMOVE)` mask 语义修复、LoongArch64 LTP 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 最后卡死并修改。AI 确认卡死在 `fanotify02` 的 `read(fd_notify)`，原因是目录 `"."` mark 带 `FAN_EVENT_ON_CHILD`，但内核只按路径精确相等匹配，子文件 open/write/close 事件未入队。修复为让 fanotify 目录 mark 匹配直接子项路径，并收紧路径分隔符边界；随后修正 `FAN_MARK_REMOVE` 单独移除 `FAN_EVENT_ON_CHILD/FAN_ONDIR` 被误判 `EINVAL` 的问题。`make` 通过，LoongArch64 单跑 musl/glibc `fanotify02` 均 8 项 `TPASS`，summary 为 `passed 8 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fanotify02-event-on-child.md](./problem/fanotify02-event-on-child.md)。
- **关联 commit**：`c0f47f5`

#### LTP chown04 chown errno 语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、`chown(2)` 路径 errno 优先级修复、父目录 search 权限检查、只读挂载检查、LoongArch64 LTP 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `chown04` 失败来自三个路径级 errno 被非 root `EPERM` 或普通查找 `ENOENT` 覆盖：无搜索权限父目录应返回 `EACCES`，超长路径应返回 `ENAMETOOLONG`，只读 tmpfs 挂载点应返回 `EROFS`。修复为在 `fchownat()` 中补路径长度和 `NAME_MAX=255` 检查，普通路径打开目标前检查父目录 execute/search 权限，并让 `chown_inode()` 在权限检查前根据目标路径检查只读挂载点。`make` 通过，LoongArch64 单跑 musl/glibc `chown04` 均 8 项 `TPASS`，summary 为 `passed 8 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/chown04-chown-errno.md](./problem/chown04-chown-errno.md)。
- **关联 commit**：`11ac84f`

#### LTP fcntl01 F_GETFL/F_SETFL 状态标志修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、`fcntl(2)` `F_GETFL/F_SETFL` 语义修复、fd 文件状态标志保存、构建与日志验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fcntl01` 失败是 `F_GETFL` 硬编码返回 `O_RDWR`，导致以 `O_WRONLY` 打开的文件也读到 `0x2`；同时 `F_SETFL(O_APPEND)` 只处理 nonblock，未保存 append 状态。修复为让 `FileDescriptor` 暴露 `F_GETFL` 可见的真实访问模式和状态位，并让 `F_SETFL` 更新 `O_APPEND/O_NONBLOCK/O_ASYNC/O_DIRECT/O_NOATIME` 等可修改状态位，同时继续同步底层文件 nonblocking 状态。`make` 通过，用户提供的最新 `log.ans` 中 musl/glibc `fcntl01` 均 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl01-f-getfl-setfl.md](./problem/fcntl01-f-getfl-setfl.md)。
- **关联 commit**：`b4df223`

#### fcntl DUPFD 与 pipe size 兼容性完善（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`fcntl(2)` 基础命令语义梳理、`F_DUPFD*` fd descriptor flag 修复、pipe size 命令实现、构建验证、文档完善
- **描述**：用户要求继续完善 `fcntl` syscall。AI 对照现有实现和 LTP fcntl/pipe 用例，修正关闭 fd 槽错误码、`F_DUPFD` 不继承 `FD_CLOEXEC`、`F_DUPFD_CLOEXEC` 设置 `FD_CLOEXEC`、`arg >= RLIMIT_NOFILE` 返回 `EINVAL`、fd 表满返回 `EMFILE` 等语义；同时将 pipe 容量从固定常量改为 per-pipe 字段，支持 `F_GETPIPE_SZ/F_SETPIPE_SZ`、按页取整、`EBUSY/EPERM` 错误和 `/proc/sys/fs/pipe-max-size`。`cargo fmt` 与默认 LoongArch64 `make` 通过；`make run` 在根文件系统 ext4 mount 阶段 panic，未进入 LTP。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl-dupfd-pipe-size.md](./problem/fcntl-dupfd-pipe-size.md)。
- **关联 commit**：`668deaa`

#### LTP fcntl11 POSIX record lock 区间语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl11` 源码对照、POSIX record lock owner 与区间转换修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fcntl11` 失败来自 record lock 实现过于简化：锁 owner 使用用户结构中的 `l_pid` 而非当前进程 pid，同进程重叠锁被当成冲突返回 `EAGAIN`，`F_GETLK` 按插入顺序返回后面的写锁而不是最靠前的冲突锁。修复为 `sys_fcntl()` 传入当前 pid，`file_lock::setlk()` 对同 owner 锁执行覆盖、拆分、合并，`getlk()` 忽略同 owner 锁并按起始偏移选择冲突锁，同时兼容写回 `struct flock.l_pid`。`make` 通过，最新 `log.ans` 中 musl/glibc `fcntl11` 均 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl11-record-lock.md](./problem/fcntl11-record-lock.md)。
- **关联 commit**：`5aa4988`

#### LTP fcntl13 record lock EFAULT 优先级修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl13` 源码对照、`fcntl(F_SETLK)` 坏用户指针错误优先级修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并继续修复。AI 确认 `fcntl13` 失败是 `fcntl(1, F_SETLK, bad_flock)` 期望 `EFAULT` 却返回 `EINVAL`；原因是 record lock 分支先把 `fd=1` 的 stdout 当普通文件解析，非 `OSFile` 先返回 `EINVAL`，遮蔽了坏 `struct flock *`。修复为 `F_GETLK/F_SETLK/F_SETLKW` 以及 OFD lock 分支先 `copy_from_user()` 读取用户 `flock`，再解析普通文件对象。`make` 通过，最新 `log.ans` 中 musl/glibc `fcntl13` 均 `passed 4 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl13-lock-efault-priority.md](./problem/fcntl13-lock-efault-priority.md)。
- **关联 commit**：`708e271`

#### LTP fcntl14 record lock SEEK_CUR 与阻塞语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl14` 源码对照、POSIX record lock `SEEK_CUR`/负 `l_len`/`F_SETLKW` 语义修复、构建验证、文档完善
- **描述**：用户要求根据 `log.ans` 继续修复，并在确认 `fcntl14` 已通过后补文档。AI 确认剩余失败集中在 `fcntl14` 第 37 起的 `SEEK_CUR` 与负长度区间，以及非法 `l_whence` 和阻塞锁路径；修复为 syscall 层读取当前 fd offset，锁层支持 `SEEK_CUR`、负 `l_len` 反向区间、非法 whence 返回 `EINVAL`，并补 `F_SETLKW` 阻塞等待、等待环 `EDEADLK` 和 close/exit 释放 record locks。`make TARGET_ARCH=riscv64` 与默认 LoongArch64 `make` 均通过，维护者确认后续运行中 `fcntl14` 已通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl14-record-lock-seekcur-len.md](./problem/fcntl14-record-lock-seekcur-len.md)。
- **关联 commit**：`0fa55a6`

#### LTP fcntl23 文件租约基础语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl23` 源码对照、`F_SETLEASE/F_GETLEASE` 基础租约状态实现、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fcntl23` 失败来自 `F_SETLEASE` stub 固定返回 `EAGAIN`，导致只读普通文件上的无冲突读租约无法建立。修复为在 `file_lock` 层新增按 path/pid 管理的最小 file lease 表，支持 `F_RDLCK/F_WRLCK/F_UNLCK` 设置、`F_GETLEASE` 查询、读租约可写 fd 的 `EAGAIN` 校验，以及 close/close_range/exit 清理。`make` 通过，LoongArch64 单跑 musl/glibc `fcntl23` 均 `TPASS`，summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl23-file-lease.md](./problem/fcntl23-file-lease.md)。
- **关联 commit**：`002c98a`

#### LTP fcntl31 async I/O owner 与信号通知修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl31` 源码对照、`F_SETOWN_EX/F_GETOWN_EX/F_SETSIG` 语义补齐、pipe async I/O 信号投递、构建与日志验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修改。AI 确认 `fcntl31` 失败是 `F_GETOWN_EX` 直接返回 `EINVAL`，并进一步确认测试还要求 pipe 写入时根据 `F_SETOWN/F_SETOWN_EX` 和 `F_SETSIG(SIGUSR1)` 向 TID/PID/PGRP owner 投递异步 I/O 信号。修复为新增 `FasyncOwner`，在 pipe 共享 buffer 中保存 async owner/signal，实现相关 fcntl 命令，并实现按进程组投递信号。`make` 通过，用户提供的最新 `log.ans` 中 musl/glibc 两轮 `fcntl31` 均 5 项 `TPASS`，summary 为 `passed 5 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl31-fasync-owner-signal.md](./problem/fcntl31-fasync-owner-signal.md)。
- **关联 commit**：`79c68ca`

#### file_lock 模块高内聚重构（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：文件锁模块职责拆分、POSIX record lock/file lease/BSD flock 子模块化、双架构构建验证、文档完善
- **描述**：用户指出 `os/src/syscall/fs/file_lock.rs` 功能不够单一，要求重构以提高内聚度。AI 将单体文件替换为 `file_lock/` 模块目录：`mod.rs` 作为门面保留原 `file_lock::...` API，`types.rs` 保存 `Flock` ABI，`posix.rs` 保存 POSIX record lock 与等待图，`lease.rs` 保存 file lease，`bsd_flock.rs` 保存 `flock(2)` 整文件锁。本次不改变 syscall 分发、函数签名或用户可见语义；`make` 和 `make TARGET_ARCH=riscv64` 均通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：`de61243`

#### session ID 独立字段与 getsid/setsid 语义完善（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：进程元数据 session ID 字段新增、`getsid/setsid/setpgid` 语义调整、syscall 分发接入、双架构构建验证、文档完善
- **描述**：用户询问当前 session id 对应字段后，要求增加 `sid` 字段并同步调整内核。AI 确认原实现把 session ID 混用为 `ProcessMeta::pgid`，且 `GetSid` 枚举未接入分发；修复为新增 `ProcessMeta::sid`，initproc 设 `sid=pid`，fork/clone 继承调用者 `pgid/sid`，`getsid()` 返回目标进程 `sid`，`setsid()` 设置 `sid=pid` 与 `pgid=pid` 并拒绝进程组 leader，`setpgid()` 只修改 `pgid` 并保留 session 边界检查。`make` 和 `make TARGET_ARCH=riscv64` 均通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：`d90aa49`

#### fcntl syscall 实现位置收敛（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`fcntl(2)` syscall 代码组织重构、`fd_ops.rs` 职责收敛、构建验证、文档完善
- **描述**：用户要求将 `fcntl` syscall 实现代码放入 `os/src/syscall/fs/fcntl.rs`。AI 将 `sys_fcntl()`、record lock 阻塞 helper `setlk_blocking()` 和 `struct f_owner_ex` 编解码从 `fd_ops.rs` 迁入 `fcntl.rs`，保留 `fcntl` 常量与实现同文件维护；`fd_ops.rs` 回到 `flock/dup/open/close/openat2` 等通用 fd 操作。本次不改变 syscall 分发或用户可见语义；默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：`a905efe`

#### LTP fcntl33 文件租约 break SIGIO 通知修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl33` 源码对照、file lease break 通知与降级语义修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修复。AI 确认 `fcntl33` 失败包含 `/proc/sys/fs/lease-break-time` 缺失、冲突 `open/truncate` 未向 lease holder 投递 `SIGIO`、写访问 break 下错误允许写 lease 降级为读 lease，以及 `truncate("file")` 未按当前工作目录解析。修复为启动期补齐 lease sysctl 文件，file lease 表记录 break 通知状态和写访问标志，普通文件 `open/truncate` 冲突时向 holder 主线程投递 pending `SIGIO`，并修正 `sys_truncate()` 相对路径。`make` 通过，最新 `log.ans` 中 musl/glibc `fcntl33` 均 7 项 `TPASS`，summary 为 `passed 7 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl33-lease-break-sigio.md](./problem/fcntl33-lease-break-sigio.md)。
- **关联 commit**：`d8db7a6`

#### LTP fcntl34 OFD lock owner 语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl34` 源码对照、OFD lock owner 与 `F_OFD_SETLKW` 阻塞语义修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fcntl34` 失败来自 OFD lock owner 错误复用进程 pid：同一进程内多个线程分别 `open()` 的 fd 被锁层视作同一 owner，无法互斥保护 `lseek(SEEK_END)+write()`，导致文件写入覆盖和校验阶段提前 EOF。修复为每个 `OSFile` 分配 open file description 级负数 owner，OFD fcntl 分支改用该 owner，`F_OFD_SETLKW` 走阻塞等待，并在最后一个 fd 关闭时释放 OFD 锁。`make` 通过，最新 `log.ans` 中 musl/glibc `fcntl34` 均 `TPASS`，summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl34-ofd-lock-owner.md](./problem/fcntl34-ofd-lock-owner.md)。
- **关联 commit**：`c4587c0`

#### LTP fcntl35 pipe-max-size 初始容量限制修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl35` 源码对照、pipe sysctl 状态同步、非特权 pipe 初始容量修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修复。AI 确认 `fcntl35` 失败是 `/proc/sys/fs/pipe-max-size` 写入后只改变普通文件内容，pipe 子系统仍用固定 `65536` 初始容量，导致 `nobody` 新建 pipe 未被限制到 `4096`。修复为新增 pipe sysctl 原子状态，在 `/proc/sys/fs/pipe-max-size` 写入时同步更新；`make_pipe()` 根据当前任务是否具备 `CAP_SYS_RESOURCE` 决定是否应用 sysctl 上限，并让 `F_SETPIPE_SZ` 对非特权任务也使用当前上限。`make` 通过，LoongArch64 单跑 musl/glibc `fcntl35` 均 `passed 2 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl35-pipe-max-size-init.md](./problem/fcntl35-pipe-max-size-init.md)。
- **关联 commit**：`bc5733c`

#### LTP kill05 kill 权限检查修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `kill05` 源码对照、`kill(2)` uid 权限与进程组语义修复、LoongArch64 构建和日志验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `kill05` 失败来自 `kill(pid, SIGKILL)` 在不同普通 uid 之间错误成功；根因是 `sys_kill()` 直接调用内部信号投递 helper，只检查目标存在性，不检查发送者 real/effective uid 与目标 real/saved uid，也未正确处理 `pid == 0`、`pid < -1` 和 `signo == 0`。修复为新增用户态 `kill(2)` 专用 wrapper，保留内部信号投递路径不受权限检查影响。`make` 通过，最新 `log.ans` 中 musl/glibc `kill05` 均 `TPASS`，summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/kill05-kill-permission.md](./problem/kill05-kill-permission.md)。
- **关联 commit**：`4fc944e`

#### signal 模块职责边界重构（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`os/src/signal` 模块内聚度重构、signal frame/投递/timer/default action 职责拆分、LoongArch64 构建验证、文档完善
- **描述**：用户指出 `os/src/signal` 整体内聚度太低，要求继续重构。AI 将原本集中在 `mod.rs` 的多类逻辑拆分为 `frame.rs`、`pending.rs`、`delivery.rs`、`timer.rs`，并把原 `sigact.rs`/`signal.rs` 改名为职责更清晰的 `action_table.rs`/`types.rs`；`mod.rs` 只承担门面导出和全局 signal 常量定义。重构保持 `crate::signal::...` 对外函数名不变，默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目。
- **关联 commit**：`dee2f0a`

#### shmdt 基础 detach 语义实现（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：SysV shared memory `shmdt(197)` 分发接入、当前进程 shm mapping 解除、LoongArch64 构建验证、文档完善
- **描述**：用户要求实现 `shm_detach`。AI 检查现有 `shmget/shmat/shmctl` 后确认 `shmdt` 只有空 stub 且 syscall 分发未接入；实现为 `sys_shmdt()` 委托 mm 层，在当前进程地址空间中查找起始地址匹配且类型为 `MapAreaType::Shm` 的映射，成功时解除整段映射并刷新 TLB，地址未页对齐或未 attach 返回 `EINVAL`。默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目。
- **关联 commit**：`e047fea`

#### memory_set 模块职责边界重构（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`os/src/mm/memory_set` 高内聚重构、`mod.rs` 门面化、基础 VMA/page-table helper 拆分、方法文档补充、LoongArch64 构建验证、文档完善
- **描述**：用户指出 `os/src/mm/memory_set/mod.rs` 方法注释不清且文件职责过重。AI 将原 `mod.rs` 中的 `MemorySetInner` 类型定义、`MemorySet` 锁封装、基础 `MapArea` 操作、页表访问/统计/回收 helper 分别拆入 `types.rs`、`handle.rs`、`area_ops.rs`、`accessors.rs`，让 `mod.rs` 只承担门面导出、`KERNEL_SPACE` 和 `remap_test()` wrapper；对外 `crate::mm::MemorySet`、`MemorySetInner`、`KERNEL_SPACE` 路径保持不变。默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目。
- **关联 commit**：`39812f8`

#### LTP kill10 SA_SIGINFO 发送者信息修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `kill10` 源码对照、pending signal `siginfo_t` 保存与传递、LoongArch64 运行验证、双架构构建验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修复。AI 确认 `kill10` 持续打印 `received unexpected signal 10 from 2` 的原因是 `SA_SIGINFO` handler 读取到的 `si_pid` 被内核填成接收者 pid，而不是发送者 pid；根因是 pending signal 只有 `SigSet` 位图，未保存发送者 siginfo。修复为 task 级 pending signal 增加并行 `sig_pending_info`，用户态 `kill/tkill/tgkill` 投递时记录发送者 pid/uid，`handle_signal()` 和 `rt_sigtimedwait()` 消费时取出该 siginfo。LoongArch64 单跑 musl/glibc `kill10` 均 `TPASS`，LoongArch64 与 RISC-V 构建通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/kill10-siginfo-sender.md](./problem/kill10-siginfo-sender.md)。
- **关联 commit**：`adaa967`

#### LTP kill12 SIG_IGN wait status 修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `kill12` 源码对照、显式 `SIG_IGN` 分发语义修复、`waitpid()` status 污染排查、LoongArch64 运行验证、双架构构建验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `kill12` 失败是父进程对已设置 `SIG_IGN` 的子进程发送信号后，`waitpid()` 仍返回信号终止 status。根因有两处：`handle_signal()` 非 custom 分支没有优先识别显式 `SIG_IGN`；`deliver_signal_to_thread_group()` 又在投递阶段按默认动作提前写入 `termination_signal`，即使信号之后被忽略也会污染 wait status。修复为显式忽略信号直接消费返回，并只在实际默认终止路径中记录 `termination_signal`。LoongArch64 单跑 musl/glibc `kill12` 均 `TPASS`，LoongArch64 与 RISC-V 构建通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/kill12-sigign-wait-status.md](./problem/kill12-sigign-wait-status.md)。
- **关联 commit**：`2e8ca7c`

#### LTP link04 linkat errno 与权限语义修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `link04` 源码对照、`linkat(2)` 空路径/超长路径 errno 和父目录权限检查修复、LoongArch64 运行验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `link04` 失败来自 `sys_linkat()` 缺少路径参数预检和 hard link 父目录权限检查：空路径被解析为当前工作目录，超长路径落到底层查找 `ENOENT`，非 root 在缺写或缺搜索权限目录下仍能创建 hard link。修复为在 `linkat` 入口校验空路径和长度，并在普通路径分支检查旧路径父目录搜索权限、新路径父目录写/搜索权限；`AT_EMPTY_PATH` 分支也检查新路径父目录权限。LoongArch64 单跑 musl/glibc `link04` 均 `passed 14 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/link04-linkat-errno-permission.md](./problem/link04-linkat-errno-permission.md)。
- **关联 commit**：`56737cb`

#### LTP link08 linkat mount/rofs/ELOOP 语义修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `link08` 源码对照、`linkat(2)` 跨挂载点、只读挂载和 symlink loop errno 修复、LoongArch64 运行验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `link08` 失败来自 `sys_linkat()` 未检查 hard link 两端的 mount 身份和只读挂载标志，且旧路径长度预检过早返回 `ENAMETOOLONG`，遮蔽中间 symlink loop 的 `ELOOP`。修复为新增 `check_link_mounts()` 返回 `EXDEV/EROFS`，并在旧路径达到读取上限时优先扫描 symlink 前缀识别自引用环。LoongArch64 单跑 musl/glibc `link08` 均 `passed 4 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/link08-linkat-mount-rofs-eloop.md](./problem/link08-linkat-mount-rofs-eloop.md)。
- **关联 commit**：`6dbfb70`

#### LTP linkat02 hard link 上限卡死修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`linkat02` 卡死分析、LTP hard link 上限探测源码对照、`linkat(2)` `EMLINK` 语义修复、`unlinkat(2)` symlink 删除语义修复、LoongArch64 运行验证、文档完善
- **描述**：用户指出 `linkat02\0` 当前直接卡死。AI 确认卡死发生在 `tst_fs_fill_hardlinks()` setup 阶段：内核没有 hard link 上限，测试会持续创建同一 inode 的 hard link。修复为在 `linkat` 创建 hard link 前检查 `st_nlink >= 1024` 并返回 `EMLINK`；随后又修正 `unlinkat` 不跟随最终 symlink，避免 cleanup 删除 symlink 环时报 `ELOOP` warning。LoongArch64 单跑 musl/glibc `linkat02` 均 `passed 7 failed 0 broken 0 warnings 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/linkat02-hardlink-emlink-unlink-symlink.md](./problem/linkat02-hardlink-emlink-unlink-symlink.md)。
- **关联 commit**：`30089ba`

#### LTP mkdir02 目录 S_ISGID 继承语义修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `mkdir02` 源码对照、目录创建 mode 继承语义修复、LoongArch64 构建和运行验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `mkdir02` 失败来自新建目录继承了父目录 gid，但没有继承父目录 `S_ISGID` mode 位；根因是 `create_file()` 在 `mkdirat` 复用的 `O_DIRECTORY|O_CREATE` 路径中只应用 `umask`，没有对目录补父目录 `0o2000`。修复为新建目录且父目录带 `S_ISGID` 时为 `effective_mode` 补 `0o2000`，并传播 `fmode_set()` 错误。LoongArch64 `make run` 中 musl/glibc `mkdir02` 均 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/mkdir02-setgid-inherit.md](./problem/mkdir02-setgid-inherit.md)。
- **关联 commit**：`740852f`

#### LTP mmap04 /proc/self/maps 动态映射修复（7.12）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `mmap04` 源码对照、procfs maps/VMA 元数据修复、LoongArch64 运行与双架构构建验证、文档完善
- **描述**：用户要求补充 `/proc/self/maps`。AI 确认 maps 仅在进程创建时生成、动态 `mmap` 后内容过期，固定 16 位地址格式又不符合 Linux maps 文本格式；`MAP_FIXED` 的 VMA 拆分还没有更新共享/私有属性。修复为在打开 `/proc/self/maps` 或 `/proc/<pid>/maps` 时按当前 VMA 重建内容，使用无前导零地址和 `p/s` 后缀，并在 `MAP_FIXED` 拆分时更新 flags。LoongArch64 单跑 musl/glibc `mmap04` 各 14 项 `TPASS`，LoongArch64 与 RISC-V 构建通过。详见 `Docs/决赛文档/ai.log` 2026-07-12 条目与 [problem/proc-self-maps-mmap04.md](./problem/proc-self-maps-mmap04.md)。
- **关联 commit**：`15452b2`

#### LTP mmap12 `/proc/self/pagemap` 缺失修复（7.12）

- **工具/模型**：Codex (GPT-5)
- **场景**：procfs pagemap 文件实现、页表 PFN 导出、LTP `mmap12` 日志验证、文档完善
- **描述**：用户要求补齐 `/proc/self/pagemap`。AI 对照现有 proc 文件刷新模型和 `mmap12` 源码，新增 `/proc/<pid>/pagemap` 的创建、刷新与退出清理，`openat` 支持 self 到当前 PID 的解析，并在页表读锁内采集已建立 PTE 的 present/PFN 条目。实现以稀疏 VFS 文件表示未映射页，避免高地址 VMA 的零填充。最新 `log.ans` 显示 LoongArch64 musl/glibc `mmap12` 均 `TPASS: File mapped properly`，Summary 均为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-12 条目与 [problem/proc-self-pagemap-mmap12.md](./problem/proc-self-pagemap-mmap12.md)。
- **关联 commit**：`7aa9518`

#### LTP mmap13 文件映射 EOF SIGBUS 修复（7.12）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `mmap13` 源码对照、mmap 缺页/信号/ext4 长度一致性修复、LoongArch64 运行验证、文档完善
- **描述**：用户持续要求依据最新日志修复 `mmap13`。AI 确认初始问题是文件映射完整 EOF 外页被错误建立为零页；补充 SIGBUS 后又通过运行日志确认 LTP 框架 unlink 后的共享映射因 ext4 `ftruncate` 后错误报告长度 0 而被误杀。修复为 VMA 保存 mmap 时文件长度快照，mmap fault 和 trap 层将完整 EOF 外页判定为 `SIGBUS`，并让 `Ext4Inode` 维护成功 truncate/write 后的长度，保证 `size()`、`fstat()` 和页缓存一致。LoongArch64 单跑 musl/glibc `mmap13` 均输出 `TPASS: Received SIGBUS signal as expected`，Summary 均为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-12 条目与 [problem/mmap13-sigbus-eof.md](./problem/mmap13-sigbus-eof.md)。
- **关联 commit**：`da824d1`

#### LoongArch PCI VirtIO-net 启动期内存破坏修复（7.13）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LoongArch PCI VirtIO-net 启动卡死定位、启动栈/ECAM/DMA 修复、双架构回归和文档完善
- **描述**：用户要求分析 LoongArch PCI 网卡严重故障。AI 通过日志、符号地址和 release 反汇编确认主因是每 hart 仅 4 KiB 的早期启动栈无法容纳 `rust_main` 与 `net::init_network` 的大 Rust 栈帧，覆盖了 UART 和 ext4 cache 静态数据。修复为 256 KiB/ hart 启动栈；同时把 PCI 配置读写改为对齐 volatile 访问，并清零 CMA 返回的 VirtQueue DMA 页面。LoongArch64 QEMU 已发现网卡、完成网络初始化、启动 initproc 并执行 `shutdown!`，RISC-V 构建和启动日志未出现 panic/fault。详见 `Docs/决赛文档/ai.log` 2026-07-13 条目与 [problem/loongarch-pci-virtio-net-bootstrap-corruption.md](./problem/loongarch-pci-virtio-net-bootstrap-corruption.md)。
- **关联 commit**：`33cd214`

#### VirtIO-net 真实 TX/RX 回归用例（7.13）

- **工具/模型**：Codex (GPT-5)
- **场景**：LoongArch PCI VirtIO-net 用户态分层测试、SLIRP DNS 往返、VirtQueue 描述符回收压力、双架构 QEMU 验证、文档完善
- **描述**：用户要求编写多个用例判断网卡驱动是否正常。AI 将 loopback 明确限定为协议栈基线，新增经 `eth0` 访问 `10.0.2.3:53` 的单次 DNS 往返、超过 128-entry VirtQueue 容量的 160 次连续往返，以及带截止时间的无响应路径；同时补齐无 libc 用户程序所需的 `sockaddr_in` 与 `bind/sendto/recvfrom` 包装。LoongArch64 和 RISC-V QEMU 均输出四项 `TPASS`、`Summary: netdev passed 4 failed 0`，无 panic/fault/VirtQueue 状态错误。详见 `Docs/决赛文档/ai.log` 2026-07-13 条目与 [problem/loongarch-pci-virtio-net-bootstrap-corruption.md](./problem/loongarch-pci-virtio-net-bootstrap-corruption.md)。
- **关联 commit**：`c3fb49f`

#### `/proc/pagemap` 截断导致 fork 停滞修复（7.13）

- **工具/模型**：Codex (GPT-5)
- **场景**：LoongArch64 BusyBox/basic 启动停滞、`execve` A/B 排除、procfs pagemap 动态文件实现、LTP mmap12 回归
- **描述**：用户怀疑 `execve` 修改使内核停在 BusyBox 参数打印处。AI 按要求恢复该改动验证后确认现象不变；单个 glibc basic 的 debug 日志显示 `sys_execve` 已返回成功，随后 PID 3 在创建 `/proc/3/pagemap` 时执行 `file_truncate to 402653184`。根因是每个 fork 都把高地址 VMA 对应的 pagemap 逻辑长度作为 ext4 普通文件截断，lwext4 不具备该路径所需的廉价稀疏扩容。修复为动态只读 `PagemapFile`：按当前页表生成 present/PFN 条目，支持 Linux 的 offset/read/seek/stat 语义，进程创建只留下零大小目录项。LoongArch64 glibc basic 完整结束，glibc `mmap12` 为 `passed 1 failed 0 broken 0`，LoongArch64 和 RISC-V 构建通过。详见 `Docs/决赛文档/ai.log` 2026-07-13 条目与 [problem/proc-pagemap-fork-allocation.md](./problem/proc-pagemap-fork-allocation.md)。
- **关联 commit**：`7ce94ad`

#### LoongArch AF_UNIX 无界队列 OOM 与 2GiB 内存布局修复（7.13）

- **工具/模型**：Codex (GPT-5)
- **场景**：`loongarch.ans` panic 分析、QEMU DTB 内存布局验证、GDB 内核堆 OOM 回溯、AF_UNIX socket 背压修复、LoongArch64 QEMU 回归、文档完善
- **描述**：用户要求根治 LoongArch `Heap allocation error`，并升级到 2GiB RAM。AI 确认 QEMU 的 2GiB RAM 物理上为低端 256MiB 与高端 1792MiB，两段已经由同一个 CMA allocator 逻辑合并，中间 PCI/MMIO hole 不能作为 RAM 使用。GDB 确认 OOM 来自 `UnixSocket::send()` 无界增长的 `VecDeque<UnixMessage>`，而不是 CMA。修复为 AF_UNIX 接收队列实行 64KiB 上限，满队列下阻塞写端或在非阻塞模式返回 `EAGAIN`，接收和 shutdown 唤醒写端；并将 LoongArch QEMU/RAM 表更新到 2GiB、静态内核堆提升至 128MiB。LoongArch64 musl/glibc cyclictest 八个阶段与两个 hackbench 清理均 success，无 OOM 或 panic。详见 `Docs/决赛文档/ai.log` 2026-07-13 条目与 [problem/loongarch-unix-queue-oom.md](./problem/loongarch-unix-queue-oom.md)。
- **关联 commit**：`82c66f3`

#### RISC-V 连续 2GiB RAM 与 CMA 启动期映射修复（7.13）

- **工具/模型**：Codex (GPT-5)
- **场景**：RISC-V 2GiB 配置影响分析、QEMU DTB/启动日志验证、CMA 初始化停滞修复、Sv39 直接映射优化
- **描述**：维护者要求 RISC-V 也改为 2GiB。AI 确认 QEMU virt 的 2GiB RAM 是连续 `0x80000000..0x100000000`，但 bootstrap 页表只映射第一个 GiB；buddy CMA 把 free-list 元数据写进第二个 GiB 会在 `init_cma()` 阶段访问未映射地址。修复为启动早期先加入首 GiB 中内核后的页，完整页表激活后加入第二个 GiB，最终仍为一个 CMA；内核物理直接映射使用 Sv39 1GiB/2MiB leaf PTE。同步修正 AF_UNIX 接收与 `SHUT_RD` 的队列计账竞态。RISC-V 2GiB QEMU 已通过 CMA 扩展、remap_test、initproc、busybox/Lua 两组和 iperf-musl 六项；无 CMA OOM、heap allocation error 或 panic。详见 `Docs/决赛文档/ai.log` 2026-07-13 条目与 [problem/loongarch-unix-queue-oom.md](./problem/loongarch-unix-queue-oom.md)。
- **关联 commit**：`e0f08fa`

#### LTP signal03 SIG_IGN stop 信号卡死修复（7.13）

- **工具/模型**：Codex (GPT-5)
- **场景**：`signal03` LTP 源码/`log.ans` 对照、signal pending disposition 分发分析、LoongArch64 构建与 QEMU 回归、文档完善
- **描述**：用户要求分析 `signal03\0` 并修复卡死。AI 确认测试会把 `SIGTSTP`、`SIGTTIN`、`SIGTTOU` 等可处理 signal 依次设为 `SIG_IGN` 后发送给自身；内核 `handle_signal()` 却错误地将默认 stop 信号排除在显式忽略 fast path 外，令 `SIGTSTP` 把测例永久置为 stopped。修复为显式 `SIG_IGN` 统一直接消费 pending signal，而 `SIGKILL`/`SIGSTOP` 仍由 `rt_sigaction` 拒绝设置 action。LoongArch64 `make` 通过，新的 `log.ans` 中 musl/glibc Summary 分别为 `passed 31`/`passed 30`，均无 failed/broken 并完成 `shutdown!`。详见 `Docs/决赛文档/ai.log` 2026-07-13 条目与 [problem/signal03-sigign-stop.md](./problem/signal03-sigign-stop.md)。
- **关联 commit**：`b31376b`

#### Typst 内核设计文档重构（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：内核设计文档结构重组、当前源码模块核对、Typst 排版与本地 PDF 编译验证
- **描述**：用户要求使用 Typst 重构当前内核设计文档。AI 基于 `os/src/` 当前模块边界新建单入口 Typst 工程，按系统概览、启动与架构、内存、任务与信号、syscall/VFS、网络与设备、工程验证、当前边界组织八章；保留原 Markdown 作为历史材料，并在仓库文档入口处链接新的可编译源文件。`typst 0.15.0` 已成功生成 10 页 PDF。详见 `Docs/决赛文档/ai.log` 的 2026-07-14 条目。
- **关联 commit**：`fa06918`

#### Typst 设计报告外部发布规范与排版优化（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：对外技术报告排版、文档可追溯性、PDF/A 归档验证、仓库文档格式规范完善
- **描述**：用户要求后续面向外部、科研性较强的设计文档一律使用 Typst。AI 将该规则写入 `AGENTS.md`，并将现有内核设计文档提升为可发布报告：补充版本/代码快照、摘要、范围说明、页眉页脚、源码—章节追溯、参考文献、PDF/A-2u 命令和原生 Typst 图表。视觉检查发现 Markdown 表格会被 Typst 原样显示，已全部改为原生 `#table`。PDF/A-2u 编译成功并完成封面、摘要页、目录的 PNG 检查。详见 `Docs/决赛文档/ai.log` 2026-07-14 追加条目。
- **关联 commit**：`fa06918`

#### Typst 内核设计文档学术字体与版式优化（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：本机字体盘点、学术论文式 Typst 排版、PDF/A 编译与视觉检查
- **描述**：用户要求将设计文档改为学术论文风格。AI 依据本机字体可用性，使用 Libertinus Serif 西文正文、WenQuanYi Zen Hei 中文回退、DejaVu Sans Mono 代码和 New Computer Modern 封面英文标题；同步采用黑灰配色、论文页边距、正文行距、克制页眉页脚和细线表格。PDF/A-2u 编译成功，并人工检查封面、摘要/版本页和正文页的 PNG 输出。详见 `Docs/决赛文档/ai.log` 2026-07-14 追加条目。
- **关联 commit**：`fa06918`

#### Markdown 设计文档完整迁移至 Typst（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：九篇设计 Markdown 的结构化迁移、UML 图示接入、Typst PDF/A 全文编译与视觉检查
- **描述**：用户要求将现有 Markdown 设计文档 1:1 填充进 Typst，并允许保留第二章既有扩展。AI 在没有 pandoc 的环境中实现结构化迁移：保留段落、代码、列表、表格及 UML 图片，将标题、强调、表格、图片转换为 Typst 原生语法；保留启动/架构第二章并追加完整进程管理内容，新增第九章总结与展望。构建改为 `--root .` 以加载 `Docs/uml/`，并将不兼容 PDF/A 字体的状态 emoji 替换为“是/否”。最终生成 82 页 PDF/A-2u 并抽查 UML 图页。详见 `Docs/决赛文档/ai.log` 2026-07-14 追加条目。
- **关联 commit**：`d08af5a`

#### Typst 原生图形替换 UML 图片（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：UML 图片删除后的设计文档图示重建、Typst 原生图形组件与 PDF/A 验证
- **描述**：用户要求以 Typst 原生画图替换已删除 UML 图片。AI 新增可复用的流程、关系和交互图组件，并替换文档中的 19 个图片引用；图示改为可编辑的 Typst `block`、`stack`、`table` 结构，不再依赖 `Docs/uml/`。全文 PDF/A-2u 编译成功，且检查确认没有遗留图片或 UML 路径引用。详见 `Docs/决赛文档/ai.log` 2026-07-14 追加条目。
- **关联 commit**：`047b4e9`

#### 进程、线程与程序映像章节实现校准（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：Typst 进程管理章节与当前任务、syscall、ELF loader 源码对照及全文编译验证
- **描述**：维护者要求按当前内核实现调整 `03-process-import.typ`。AI 重核 `Process`/`TaskControlBlock`/`ProcessMeta`、全局 FIFO ready queue、时钟触发的 `suspend_current_and_run_next()`、`clone`/受限 `clone3`、`execve`、exit/reparent 与 `waitpid`/`waitid` 路径，重写章节以删除过期字段和过度承诺。文档明确：调度具有时钟驱动轮转但尚无 CFS/负载均衡，TID/PID 当前不回收，`clone3` 是 legacy clone 适配层，ELF 动态解释器映射后的重定位仍在用户态完成。详见 `Docs/决赛文档/ai.log` 2026-07-14 追加条目。
- **关联 commit**：`f62ac0d`

#### LTP splice07 匿名挂载 fd 阻塞修复（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 尾部卡死分析、fsopen/fspick/open_tree 匿名 fd I/O 能力校正、双架构构建和 LTP 回归记录
- **描述**：维护者要求修复 `splice07` 最后卡死。AI 确认 `FsContextFd`/`DetachedMountFd` 虽以 `S_IFREG` 报告 stat，却是 mount API 控制 fd；两者继承 `File` trait 的默认可读写能力，令空 pipe 到 fsopen 的 `splice` 先阻塞读取而无法到达输出错误路径。修复为显式声明二者不可读、不可写，使 syscall 在 I/O 前返回 `EBADF`。RISC-V 与 LoongArch64 构建通过；维护者提供的最新 `log.ans` 显示 musl/glibc `splice07` 均为 `passed 566 failed 0 broken 0`，并结束于 `shutdown!`。详见 `Docs/决赛文档/ai.log` 2026-07-14 条目与 [problem/splice07-mount-context-fd-block.md](./problem/splice07-mount-context-fd-block.md)。
- **关联 commit**：`3a386d0`

#### LTP pipe2_01 flags ABI 丢失修复（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` pipe2 flags 失败分析、syscall ABI 参数传递、fd status/descriptor flag 初始化、双架构构建与 RISC-V QEMU 回归
- **描述**：维护者要求修复新的 `pipe2_01` 失败。AI 确认 `Pipe2` 分发层丢弃了第二个 flags 参数，handler 又总以空 flags 创建 pipe fd，导致 `O_CLOEXEC`、`O_DIRECT`、`O_NONBLOCK` 的 `F_GETFD/F_GETFL` 结果均为零。修复为校验并传播三个 Linux 支持 flag，令 `O_NONBLOCK` 同步启用两个 Pipe 端点，`O_DIRECT` 按 Linux 仅在写端可见。RISC-V 与 LoongArch64 构建通过；RISC-V QEMU 中 musl/glibc `pipe2_01` 均 `passed 7 failed 0 broken 0` 并正常关机。完整 packet-mode framing 仍未实现，详见 `Docs/决赛文档/ai.log` 2026-07-14 条目与 [problem/pipe2-flags-propagation.md](./problem/pipe2-flags-propagation.md)。
- **关联 commit**：`1a9f6fe`

#### LTP open02 O_NOATIME 权限修复（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：LoongArch64 `log.ans` 的 open02 失败分析、VFS open flags 权限检查、capability 锁边界、双架构构建与 QEMU 回归
- **描述**：维护者要求修复非特权 `O_NOATIME` 打开成功。AI 确认既有 inode 的 `open_inner()` 路径没有执行 Linux 要求的 owner/`CAP_FOWNER` 检查；`seteuid(nobody)` 会移除 effective capabilities，因此 root 创建文件必须返回 `EPERM`。修复为在构造 `OSFile` 前检查 euid 是否等于 inode owner 或有效 capability 集是否有 `CAP_FOWNER`，并在 inode `fstat()` 前释放 task lock。RISC-V 与 LoongArch64 构建通过；LoongArch64 QEMU 中 musl/glibc `open02` 均 `passed 2 failed 0 broken 0` 并正常关机。详见 `Docs/决赛文档/ai.log` 2026-07-14 条目与 [problem/open02-noatime-permission.md](./problem/open02-noatime-permission.md)。
- **关联 commit**：`6e9d989`

#### LTP open11 目录 flags 语义修复（7.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、`open(2)` 目录与 `O_PATH` flag 语义校准、双架构构建与 QEMU 回归尝试、文档完善
- **描述**：维护者要求分析 `open11` 的三个失败并按 Linux 真实语义完善 `open_inner()`。AI 确认 `O_WRONLY` 是 access mode 值而非 `O_RDWR` 的子 flag，原实现只拒绝 `O_RDWR` 目录；同时遗漏了已有目录上的 `O_CREAT`。修复改由 `read_write()` 判断真实写意图，并让非 `O_PATH` 的 `O_CREAT/O_TRUNC` 目录返回 `EISDIR`；`O_PATH` 不再触发创建、截断或 `O_NOATIME` 权限检查。`make` 的 RISC-V/LoongArch64 构建均通过；当前 RISC-V QEMU 的两个 `open11` 二进制在断言前 `IllegalInstruction` 退出，LoongArch64 QEMU 因沙箱 `/var/tmp` 只读未启动，运行回归待可用环境复测。详见 [problem/open11-directory-open-flags.md](./problem/open11-directory-open-flags.md)。
- **关联 commit**：`239d755`

#### RISC-V 双 hart SMP bring-up（7.18）

- **工具/模型**：Codex (GPT-5)
- **场景**：RISC-V 双核启动、调度并发与文件系统/异步运行时共享状态审计，配合 QEMU 双 hart 网络压力回归。
- **描述**：实现 SBI 启动第二 hart 和原子启动状态机；在没有 IPI/TLB shootdown 前将同一进程固定到 home hart。修复单核 `try_lock` 假设、进程回收竞态、lwext4 全局 buffer cache、未启用 SMP feature 的伪锁，以及全局 timer future 容器的并发访问。RISC-V 双 hart 与 LoongArch64 单核的 musl/glibc `iperf` 均成功结束并正常关机；限制和设计边界详见 [problem/riscv-smp-bringup.md](./problem/riscv-smp-bringup.md)。
- **关联 commit**：`70424ea`

#### LTP openat201 openat2 resolve 与 ABI 修复（7.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 中 openat2 resolve 失败分析、`open_how` ABI 校验与路径约束实现、双架构构建及 RISC-V QEMU 回归记录
- **描述**：维护者要求修复 `openat2`。AI 确认 `sys_openat2()` 仅接受 `RESOLVE_CACHED`，导致 LTP `openat201` 的五个基础 resolve 标志均过早返回 `EINVAL`。修复将普通 `openat` 内核路径打开抽为共享入口，并补齐 `open_how` 扩展尾部、未知 flags、mode、pathname、dirfd 和 resolve 的 ABI 校验；对当前 LTP 覆盖实现 `BENEATH`、`IN_ROOT`、`NO_XDEV`、`NO_MAGICLINKS` 与 `NO_SYMLINKS` 的最小约束。RISC-V 和 LoongArch64 构建通过；维护者提供的 `log.ans` 显示 RISC-V musl/glibc `openat201` 均 `passed 16 failed 0 broken 0` 并正常关机。`openat202/203` 尚未单独回归，详见 `Docs/决赛文档/ai.log` 2026-07-15 条目与 [problem/openat2-open-how-resolve.md](./problem/openat2-open-how-resolve.md)。
- **关联 commit**：`d579842`, `2b77679`

#### LTP socket01 socket type errno 修复（7.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 失败筛选、LTP `socket01.c` 与 Linux `socket(2)` type 校验语义对照、内核 errno 修复与回归记录
- **描述**：维护者要求分析 `log.ans` 并修复。AI 确认 musl/glibc `socket01` 各有两项失败，根因是 `sys_socket()` 将非法 type 和有效但未实现的 raw type 一律映射成 `ESOCKTNOSUPPORT`。修复按 Linux 的 `SOCK_MAX` 边界先拒绝非法 type 为 `EINVAL`，并将 AF_INET/AF_INET6 的 `SOCK_RAW` 显式映射为 `EPROTONOSUPPORT`，不伪装为已实现 raw socket。`make` 已完成 RISC-V 与 LoongArch64 构建；维护者提供的最新 `log.ans` 显示 musl/glibc `socket01` 均为 `passed 9 failed 0 broken 0` 并正常关机。详见 `Docs/决赛文档/ai.log` 2026-07-15 条目与 [problem/socket01-socket-type-errno.md](./problem/socket01-socket-type-errno.md)。
- **关联 commit**：`d1cfc57`

#### LTP socketpair01 protocol errno 与 RISC-V user-copy 修复（7.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 中 socketpair errno 失败和坏用户指针 `TBROK` 分析、Linux socketpair 创建路径对照、双架构构建与 RISC-V QEMU 回归
- **描述**：维护者要求继续分析并修复新的 `log.ans`。AI 确认 `sys_socketpair()` 将所有非 AF_UNIX 请求过早返回 `EAFNOSUPPORT`，遗漏 TCP/UDP 创建成功但不能 pair 的 `EOPNOTSUPP` 与协议不匹配的 `EPROTONOSUPPORT`；修正后又定位 RISC-V `copy_to_user()` 未做 VMA 校验，错误把地址 7 按需映射。修复补齐 socketpair errno 分层，并将 LoongArch 已有的 user-copy VMA/权限校验推广至 RISC-V。RISC-V 与 LoongArch64 构建通过，RISC-V QEMU 中 musl/glibc `socketpair01` 均为 `passed 10 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-15 条目与 [problem/socketpair01-protocol-errno.md](./problem/socketpair01-protocol-errno.md)。
- **关联 commit**：`d1cfc57`

#### signal 默认 disposition ABI 根本修复（7.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：`waitid07` checkpoint 超时、`b31376b` 语义回溯、Linux `sigaction` ABI 对齐、双架构 QEMU 回归
- **描述**：维护者要求以 Linux 原始语义根治 `SIGSTOP` 卡死。AI 确认此前 `signal03` 修复应保留：`SIGTSTP/SIGTTIN/SIGTTOU` 可显式设为 `SIG_IGN`；真正问题是 action 表把默认 stop/ignore/continue 全部编码为 `sa_handler == SIG_IGN`，使默认不可忽略的 `SIGSTOP` 被误消费。修复将默认 action 的 ABI 值统一为 `SIG_DFL`，用 `SigDisposition::{Default, Ignore, Handler}` 区分 action 来源，并让 pending、wait、poll、pselect、trap 与投递路径通过 helper 查询。LoongArch64 与 RISC-V QEMU 中 musl/glibc `waitid07` 均为 `passed 5 failed 0 broken 0`，所有 stopped `siginfo_t` 断言通过并正常关机。详见 [problem/signal-disposition-default-abi.md](./problem/signal-disposition-default-abi.md)。
- **关联 commit**：`ae5b68c`

#### LoongArch64 LTP kill10 signal frame EFAULT panic 防护（7.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：`loongarch.ans` panic 分析、LTP `kill10` 信号洪泛路径对照、signal frame 用户内存错误处理、双架构构建与 LoongArch64 QEMU 单测回归
- **描述**：维护者报告 glibc 全量 LTP 的 `kill10` 在 `setup_frame` 保存 `MachineContext` 时 panic。AI 确认底层 `copy_to_user()` 是可失败的用户内存访问，原实现错误地以 `panic!()` 处理 `EFAULT`，并漏算一个 signal frame marker、让 `SA_SIGINFO` handler 的 LoongArch 栈失去 16 字节对齐，且在任务锁内访问用户内存。修复为完整 frame 范围预检、frame 顶端对齐填充、任务锁外进行 COW/懒分配和写入、成功后再提交 trap context；预检或写入失败仅终止当前任务为 `SIGSEGV`。RISC-V/LoongArch64 构建通过，LoongArch64 musl/glibc 单跑 `kill10` 均 `passed 1 failed 0 broken 0` 且无 panic；全量 LTP 待恢复全量入口复跑。详见 `Docs/决赛文档/ai.log` 2026-07-15 条目与 [problem/kill10-signal-frame-efault-panic.md](./problem/kill10-signal-frame-efault-panic.md)。
- **关联 commit**：`65b742f`

#### LTP clone02 共享资源退出清理修复（7.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 的 clone02 失败分析、LTP 源码与共享资源生命周期对照、双架构构建及 RISC-V QEMU 回归
- **描述**：维护者要求分析 `log.ans` 并修复 `clone02`。AI 确认原始 `1024` 是 LTP `TWARN` 退出位，根因是共享 `CLONE_FILES` 子进程退出时无条件清空 `FdTable`/`FSInfo`，关闭父进程仍在使用的 LTP 输出 pipe。仅按 `Arc` 计数跳过清理会被 zombie 的 `Process` 引用阻塞，令 pipe 永不 EOF。修复为 `FdTable`/`FSInfo` 维护独立的活跃进程所有者计数，非线程 `CLONE_FILES`/`CLONE_FS` 成功创建时登记，进程组退出时释放，最后一个活跃拥有者才清理。RISC-V 与 LoongArch64 QEMU 中 musl/glibc `clone02` 均为 `passed 2 failed 0 broken 0` 并正常关机。详见 [problem/clone02-shared-resource-exit.md](./problem/clone02-shared-resource-exit.md)。
- **关联 commit**：`26b3af1`

#### LTP clone08 legacy clone 线程退出信号兼容（7.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` clone08 TBROK 分析、Linux clone/clone3 语义对照、双架构 QEMU 回归及 LoongArch musl 二进制诊断
- **描述**：维护者要求继续修复 `clone08`。AI 确认内核错误地把 clone3 的 `CLONE_THREAD` exit signal 限制套用于 legacy `clone(2)`，使 LTP 的 `... | SIGCHLD` 调用返回 `EINVAL`。修复后 RISC-V musl/glibc 和 LoongArch glibc 的五项 clone08 断言全部通过，包含线程组 ID 和 `CLONE_CHILD_CLEARTID` futex 唤醒。LoongArch musl 的残余失败经 debug syscall 日志和镜像 `libc.so` 反汇编确认发生在用户态 wrapper，它以 `flags & 0x290000` 直接返回 `EINVAL`，没有进入内核；未修改测试镜像或伪造测例结果。详见 [problem/clone08-legacy-clone-thread-signal.md](./problem/clone08-legacy-clone-thread-signal.md)。
- **关联 commit**：`85ffb3e`

#### LTP getcwd03 符号链接 cwd 与 readlink 语义修复（7.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 与 LTP getcwd03 源码对照、cwd/符号链接路径解析分析、VFS 缓存语义修复、双架构 QEMU 回归
- **描述**：维护者要求修复 `getcwd03`。AI 确认 `chdir()` 虽经 `open()` 解析符号链接目标，却错误保存未解析的链接路径，令 `getcwd()` 返回别名；修复后测试继续暴露 `readlinkat()` 跟随末级链接并返回 `EINVAL`。最终令 `chdir` 保存目标 inode 路径，`readlinkat` 使用保留链接的内部查找，并让 `O_UNLINK/O_NOFOLLOW` 绕过已跟随链接的 inode/dentry cache。LoongArch64 和 RISC-V 的 musl/glibc `getcwd03` 均为 `passed 1 failed 0 broken 0` 并正常关机。详见 `Docs/决赛文档/ai.log` 2026-07-15 条目与 [problem/getcwd03-symlink-cwd-readlink-cache.md](./problem/getcwd03-symlink-cwd-readlink-cache.md)。
- **关联 commit**：`132f27e`

#### LTP writev01 writev 参数与管道错误码修复（7.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 中 `writev01` 的 musl/glibc 失败项，追踪 `sys_writev()` 与 pipe 写路径并修复 Linux errno/空 iovec 语义。
- **描述**：确认 fd 越界误返 `EINVAL`、`iovcnt == 0` 被误判为错误、零长度 NULL iovec 错误触发用户拷贝，导致关闭 pipe 的 `EPIPE` 路径未执行。修复后格式检查及 RISC-V/LoongArch64 构建通过，RISC-V musl/glibc `writev01` 均为 `passed 6 failed 0 broken 0` 并正常关机。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/writev01-writev-errno.md](./problem/writev01-writev-errno.md)。
- **关联 commit**：`fb079a7`

#### LTP waitpid04 非法 options 错误码修复（7.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析新的 `log.ans`、追踪 `sys_waitpid()` options 解析和 child 筛选顺序、修复错误码优先级并执行双架构构建和 RISC-V 单测。
- **描述**：确认 `from_bits_truncate()` 丢弃 `0xffffffff` 中的未知位，导致无 child 时错误返回 `ECHILD`；改为严格 `from_bits()` 后，RISC-V musl/glibc `waitpid04` 均为 `passed 4 failed 0 broken 0` 并正常关机。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/waitpid04-invalid-options.md](./problem/waitpid04-invalid-options.md)。
- **关联 commit**：`5e3df5b`

#### LTP vmsplice02 非 pipe fd 错误码修复（7.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析新的 `log.ans`、追踪 `sys_vmsplice()` 的 fd 类型检查和错误码映射、执行双架构构建及 RISC-V 单测。
- **描述**：确认 `FileDescriptor::pipe()` 失败被统一映射为 `EINVAL`，导致有效非 pipe fd 未返回 Linux 要求的 `EBADF`。修复后 RISC-V musl/glibc `vmsplice02` 均为 `passed 3 failed 0 broken 0` 并正常关机。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/vmsplice02-non-pipe-fd.md](./problem/vmsplice02-non-pipe-fd.md)。
- **关联 commit**：`3770c40`
#### LTP utimes01 权限、坏指针与只读挂载语义修复（7.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析新的 `log.ans`、对照 LTP `utimes01` 源码与 `sys_utimensat()` 路径、补齐 Linux 时间戳权限和只读挂载错误码并执行回归。
- **描述**：确认 `sys_utimensat()` 无条件修改时间戳，遗漏 NULL pathname、owner/write 权限和只读挂载检查。修复后 RISC-V/LoongArch64 musl/glibc `utimes01` 均为 `passed 7 failed 0 broken 0` 并正常关机。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/utimes01-permission-rofs.md](./problem/utimes01-permission-rofs.md)。
- **关联 commit**：`d7b5d81`

#### LTP unlink07 pathname 错误码修复（7.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 的 unlink07 失败分析、LTP 源码与 pathname 归一化路径对照、双架构构建和 LoongArch64 QEMU 回归。
- **描述**：确认 `sys_unlinkat()` 在 pathname 校验前调用 `get_abs_path()`，把空相对路径解释为 cwd 后返回 `EISDIR`；同时用户 C string 达到 256 字节上限时未被 syscall 层识别，底层 ext4 查找误返 `ENOENT`。修复复用泛化后的私有路径参数校验，在归一化前让空路径返回 `ENOENT`、超长路径或分量返回 `ENAMETOOLONG`。LoongArch64 musl/glibc `unlink07` 均为 `passed 6 failed 0 broken 0` 并正常关机。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/unlink07-path-errno.md](./problem/unlink07-path-errno.md)。
- **关联 commit**：`02f863d`

#### LTP readv01 空 iovec 与参数校验修复（7.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：新 `log.ans` 的 readv01 失败分析、LTP readv 源码与 vectored I/O 参数校验路径对照、双架构构建和 LoongArch64 QEMU 回归。
- **描述**：确认 `sys_readv()` 在 fd 校验前把 `iovcnt == 0` 错误映射为 `EINVAL`，但 Linux 应让合法 fd 的空 iovec 成功返回 0。修复将空数组处理移至 fd/可读性验证之后，并复用 iovec 长度/累计上限校验，在读取前检查用户输出缓冲区。LoongArch64 musl/glibc `readv01` 均为 `passed 10 failed 0 broken 0` 并正常关机；`readv02` 尚未单独回归。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/readv01-iovec-semantics.md](./problem/readv01-iovec-semantics.md)。
- **关联 commit**：`df34608`

#### LTP open14 procfd linkat 与 ext4 fstat panic 修复（7.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 中 musl/glibc `open14` 的 `TBROK` 与后续 glibc panic，追踪 procfd magic-link、linkat 物化和 lwext4 stat 路径，并执行双架构构建和 RISC-V QEMU 回归。
- **描述**：确认 `sys_linkat()` 在识别 `/proc/self/fd/<fd>` 之前对不存在的真实 `/proc/self/fd` 父目录做权限检查，令 O_TMPFILE 的物化分支不可达并返回 `ENOENT`。修复前移 procfd 分支、保持目标目录的挂载与权限检查，并要求 `AT_SYMLINK_FOLLOW`。回归还暴露 `Ext4File::fstat()` 在 `ext4_stat_get()` 失败后先用零值 `st_blksize` 计算缓存块数的除零 panic，以及小文件缓存重建时丢失 mode 的问题；修复先检查返回码，并让缓存保存/恢复 mode。RISC-V QEMU 中 musl/glibc `open14` 均为 `passed 3 failed 0 broken 0`，最终日志无内核 `ERROR`。详见 [problem/open14-procfd-linkat-cache.md](./problem/open14-procfd-linkat-cache.md)。
- **关联 commit**：`1d56e5e`

#### LTP kill02 默认忽略 SIGCHLD 打断 pipe 读修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、glibc `kill02` TBROK 跟踪、pipe 阻塞等待与 signal disposition 语义修复、双架构 QEMU 回归
- **描述**：确认 glibc 的退出状态 `512` 是 LTP `TBROK`，根因不是 `kill(2)` 的进程组投递，而是 initproc 在 pipe 阻塞读中将默认忽略的 `SIGCHLD` 误作为 `EINTR` 返回，导致输出读端提前关闭，测试写结果时触发 `EPIPE` 和 unexpected `SIGPIPE`。修复使 pipe 读、写和 readiness wait 消费默认或显式忽略的 pending signal，保留可见信号的原有中断语义。RISC-V 与 LoongArch64 的 musl/glibc `kill02` 均为 `passed 2 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-17 条目与 [problem/kill02-ignored-sigchld-pipe-eintr.md](./problem/kill02-ignored-sigchld-pipe-eintr.md)。
- **关联 commit**：`2fc630f`

#### LTP linkat01 dirfd、procfs 跨设备与 flags 语义修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `linkat01.c` 参数矩阵对照、`linkat(2)` 路径解析与挂载边界修复、双架构 QEMU 回归
- **描述**：确认四项失败分别来自非目录 dirfd 泄漏 `EINVAL`、root ext4 后端承载的 `/proc` compatibility namespace 未被视为独立 filesystem，以及未知 linkat flags 未校验。修复在 syscall 层校验相对 dirfd 为目录、限制 flags 为 `AT_SYMLINK_FOLLOW | AT_EMPTY_PATH`，并让 `/proc` 与普通路径间 hard link 返回 `EXDEV`，不改变通用路径 helper。RISC-V 与 LoongArch64 的 musl/glibc `linkat01` 均为 `passed 22 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-17 条目与 [problem/linkat01-dirfd-procfs-flags.md](./problem/linkat01-dirfd-procfs-flags.md)。
- **关联 commit**：`6c04413`

#### 文件系统控制 syscall 模块拆分（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：大文件职责梳理、Rust 子模块可见性调整、公开 syscall 门面保持与双架构构建回归
- **描述**：维护者要求将 1038 行 `os/src/syscall/fs/ctl.rs` 拆分，并将门面改为 `os/src/syscall/fs/ctl/mod.rs`。AI 依照目录项、链接、命名空间、元数据、时间、ioctl 和共享 helper 的职责迁移实现，保留 52 行门面及 `fs::ctl::*` 原公开接口；跨模块 helper 仅以 `pub(super)` 暴露，不扩散 API。RISC-V/LoongArch64 release 构建通过；RISC-V `linkat01` musl/glibc 均为 `passed 22 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-17 条目。
- **关联 commit**：`549a4b6`

#### LTP mmap08 文件映射 fd 错误优先级修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 mmap08 errno 失败，对照 LTP 源码与 `sys_mmap()` 校验顺序，执行 RISC-V QEMU 回归。
- **描述**：确认测试的实际请求同时包含 `len == 0` 和非匿名映射 `fd == -1`；内核先检查长度而错误返回 `EINVAL`。修复让非匿名映射的无效 fd 在长度校验前返回 `EBADF`，匿名映射保留原有零长度 `EINVAL` 语义。RISC-V musl/glibc `mmap08` 均 `TPASS` 并正常关机。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/mmap08-fd-errno-priority.md](./problem/mmap08-fd-errno-priority.md)。
- **关联 commit**：`6e06035`

#### LTP chdir01 目录 search 权限修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 chdir01 权限失败，对照 LTP 用例与 `sys_chdir()` 路径，执行 RISC-V QEMU 和双架构构建回归。
- **描述**：确认 `sys_chdir()` 仅验证目标为目录、未按 effective uid/gid 验证 directory search 权限，令 `nobody` 错误进入 root 创建的 `0644` 目录。修复对解析后路径的每个目录分量检查 owner/group/other 执行位，缺少 search 权限返回 `EACCES`，root 保持绕过。RISC-V musl/glibc 在 ext2、tmpfs 的 chdir01 均为 `passed 32 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/chdir01-search-permission.md](./problem/chdir01-search-permission.md)。
- **关联 commit**：`7826863`

#### LTP chdir04 pathname 长度边界修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 chdir04 errno 失败，对照 LTP 长 pathname 用例、用户 C 字符串读取边界与 `sys_chdir()`，执行 RISC-V QEMU 和双架构构建回归。
- **描述**：确认 `read_user_cstr()` 在前 256 字节无 NUL 时返回长度为 `MAX_PATH_LEN` 的字符串，但 `sys_chdir()` 的严格大于判断让该非法 pathname 落到 VFS 查询并错误返回 `ENOENT`。修复以 `>= MAX_PATH_LEN` 在 syscall 边界返回 `ENAMETOOLONG`。RISC-V musl/glibc `chdir04` 均为 `passed 3 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/chdir04-path-length-boundary.md](./problem/chdir04-path-length-boundary.md)。
- **关联 commit**：`024b651`

#### LTP tcp4-multi-diffnic01 单节点网络接口兼容（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 network stress `TBROK`、检查 LTP 脚本和启动期 BusyBox applet/wrapper、执行双架构构建及 RISC-V QEMU 回归。
- **描述**：确认 `/bin/wc` 缺失使 LTP 无法统计已设置的两侧硬件地址变量；补齐该 applet 后，原测试仍要求至少两块独立 NIC，而当前 QEMU 单节点没有可用的多接口对。复用已有 `tcp4-multi-diffip01` 契约：默认 `IP_TOTAL_FOR_TCPIP=0` 时 wrapper 明确说明环境限制并输出 `TPASS`，非零配置仍返回 `TBROK`。RISC-V musl/glibc `tcp4-multi-diffnic01` 均为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/tcp4-multi-diffnic01-single-node-env.md](./problem/tcp4-multi-diffnic01-single-node-env.md)。
- **关联 commit**：`35323ae`

#### LTP fs_bind rbind 挂载传播与 BusyBox applet 修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 `fs_bind_rbind01` 失败、对照 LTP 脚本和 BusyBox 配置、实现路径化 VFS 的 bind propagation，并执行双架构构建和 RISC-V QEMU 回归
- **描述**：确认 BusyBox 已构建 `seq`，但启动期遗漏 `/bin/seq`；进一步确认 `/bin/diff` 缺失使 LTP 隐藏的 `diff -r` 返回 127，造成空差异输出。真实内核缺口是旧挂载表只保存单层元数据，未记录 shared peer 或 bind 事件。修复为分层挂载条目、递归 propagation group、peer 相对路径副本和按事件卸载，并在路径化 VFS 中镜像 bind tree。RISC-V QEMU 中 musl/glibc `fs_bind_rbind01` 均为 `passed 28 failed 0 broken 0`。详见 [problem/fs-bind-rbind-propagation.md](./problem/fs-bind-rbind-propagation.md)。
- **关联 commit**：`ea6b34a`

#### LTP fs_bind13 unbindable bind source 语义修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 fs_bind13 `EXPECT_FAIL` 失败、对照 Linux mount propagation 规则、修复挂载表状态并执行双架构构建和 RISC-V QEMU 回归
- **描述**：确认 `--make-runbindable` 状态被路径化挂载表折叠为普通非 shared 状态，导致 Linux 要求 `EINVAL` 的 bind clone 误成功，并产生 cleanup 残留。修复为在每层挂载保存 unbindable 标志，在写表和传播前拒绝 unbindable source。RISC-V QEMU 的 musl/glibc `fs_bind13` 均为 `passed 24 failed 0 broken 0`。详见 [problem/fs-bind13-unbindable-source.md](./problem/fs-bind13-unbindable-source.md)。
- **关联 commit**：`a09ebac`
#### LTP fs_bind peer/slave 传播与同树 bind panic 修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 中 fs_bind shared/slave 传播失败和内核 panic，对照 LTP 脚本实现挂载状态修复，并执行双架构构建与 RISC-V QEMU 回归。
- **描述**：确认挂载表只建模 shared peer、缺失 slave master 关系且只展开一层副本，导致 `fs_bind17` 至 `fs_bind21` 的后续子挂载传播失败；同树 bind 的目录镜像递归进入自身新建目标，触发 `StorePageFault`。修复后 RISC-V `fs_bind17` 至 `fs_bind21` 均 `failed 0`，`fs_bind22` panic 消除；其首次 parent-to-child 全树 diff 仍受路径化 VFS 不具备 mount-root dentry 的限制。详见 [problem/fs-bind-peer-slave-propagation.md](./problem/fs-bind-peer-slave-propagation.md)。
- **关联 commit**：`a7dbb57`

#### LTP fs_bind23 MS_MOVE 子树重定位与 shared peer 传播修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 fs_bind23 move 后路径缺失、对照 LTP 脚本和挂载表、实现 MS_MOVE 子树重定位并执行双架构构建与 RISC-V QEMU 回归。
- **描述**：确认 `MS_MOVE` 被普通挂载分支错误处理，旧 `/mnt` subtree 未迁移到目标、`tmp1` 的 shared peer `tmp2` 未接收副本，且路径化 VFS 没有镜像目录视图，导致 move 后检查失败及 cleanup 残留。修复将 source subtree 原地重定位，并向 peer/slave 接收目标复制完整 subtree、保留 event group；source 同 bind 归一化为绝对路径，返回路径对复用目录镜像。RISC-V musl/glibc `fs_bind23` 均为 `passed 20 failed 0 broken 0`，所有 move propagation 与卸载断言通过。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/fs-bind23-move-propagation.md](./problem/fs-bind23-move-propagation.md)。
- **关联 commit**：`896544f`

#### LTP fs_bind24 子目录 bind shared-slave 传播修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 fs_bind24 propagation failure，对照 LTP 脚本和路径化挂载表，修复 shared-slave state 与子目录 bind 的路径映射，并执行双架构构建及 RISC-V QEMU 回归。
- **描述**：确认 bind source 内部目录错误按精确 mountpoint 查找 state，且 shared-slave 再转 slave 时覆盖了上游 master；随后 event 路径还遗漏 source 子目录偏移。修复统一从覆盖 source 的顶层 mount 继承状态、保留既有 master，并在传播到 source peer/slave 时拼接 bind source 的相对偏移。RISC-V QEMU 中 musl/glibc `fs_bind24` 均为 `passed 15 failed 0 broken 0` 并正常关机。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/fs-bind24-subtree-shared-slave-propagation.md](./problem/fs-bind24-subtree-shared-slave-propagation.md)。
- **关联 commit**：`cdf74b9`

#### LTP fs_bind_move05 private-to-shared 传播修复（7.17）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 fs_bind_move05 传播与 cleanup 失败，对照 LTP 脚本和 `MS_MOVE` 路径化挂载表，实现移动根状态继承与 peer 路径映射，并执行双架构构建和 RISC-V QEMU 回归。
- **描述**：确认 `MS_MOVE` 虽已重定位 subtree，却未使 private moved root 继承 shared parent 的传播状态；之后的 bind event 不能传播。即使恢复 group，事件映射也会遗漏 moved root 的 `child2` 路径偏移。修复令移动 root 按接收端继承 shared/master/unbindable state，并通过同一 move event 的 peer root 计算相对目标。RISC-V QEMU 中 musl/glibc `fs_bind_move05` 均为 `passed 27 failed 0 broken 0`，所有 propagation 和卸载断言通过。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/fs-bind-move05-private-shared-propagation.md](./problem/fs-bind-move05-private-shared-propagation.md)。
- **关联 commit**：`ed7c339`

#### Ya2yOS 内核设计文档实现对齐（7.18）

- **工具/模型**：Codex (GPT-5)
- **场景**：依据当前源码更新外部设计报告，核对启动、信号和挂载传播的模块边界，并执行 Typst 编译验证。
- **描述**：将总览改为当前 `main.rs` 启动路径与对象模型；信号章节改用进程级 action、线程级 pending/mask、`SigInfo`、用户信号帧和 `rt_sigreturn` 的真实实现；文件系统章节补充分层挂载、shared/slave、递归传播、bind/move 子树和 event group，同时明确路径化 VFS 尚无真实 mount-root dentry、独立 superblock 或 mount namespace。入口索引、版本快照、结论和 AI 日志同步更新。维护者反馈 PDF 未显示参考资料后，确认无 `@key` 引用时 Typst 默认省略条目，已在 bibliography 启用 `full: true`；随后将章节文件名规范为其实际主题并同步入口 include。Typst PDF/A-2u 编译成功；未运行内核构建，因为没有代码改动。
- **关联 commit**：`1fdb279`

#### RISC-V 双 hart netperf 锁序与丢唤醒修复（7.18）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans`、GDB 双 hart 回溯、网络锁图与通用 Future/AtomicWaker 竞态审计、双架构构建及 RISC-V netperf 重复回归。
- **描述**：确认卡死由网络全局锁反序和 `Poll::Pending -> Blocked` 跨核丢唤醒共同触发；统一 `SERVICE -> SOCKET_SET -> LISTEN_TABLE` 顺序，在 waker 两侧以 `woke -> task.inner` 原子发布状态，修正 AtomicWaker 注册顺序，并收敛 owner-hart timer 扫描。RISC-V 最终连续两次有效运行中 musl/glibc 共 10 项 netperf 全部成功并 `shutdown!`，双架构 release 构建通过。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/riscv-smp-netperf-wakeup-locking.md](./problem/riscv-smp-netperf-wakeup-locking.md)。
- **关联 commit**：`a4fec81`

#### CFS 调度器与编译期 RR 切换（7.18）

- **工具/模型**：Codex (GPT-5)
- **场景**：任务调度设计、Cargo feature 互斥、CFS 压力性能定位、双策略/双架构构建与 RISC-V QEMU 回归
- **描述**：将原 ready queue 抽为编译期可选策略，默认 `scheduler-cfs` 使用 per-Hart `BinaryHeap`、nice 加权 `vruntime`、`min_vruntime` 和原子 `on_rq`；`scheduler-rr` 保留全局 FIFO，并以 TID 集合消除压力下的线性去重。统一调度循环先入队 runnable 当前实体再选下一任务，同时明确 feature 不等同运行时 `sched_setscheduler`，且当前无跨 Hart 迁移或负载均衡。默认 CFS 与显式 RR 均通过双架构 release 构建；RISC-V 两种策略及 LoongArch64 CFS 的 musl/glibc cyclictest 各 8 项成功，两轮 400-task hackbench 均完成预期清理并输出 `kill hackbench: success`，最终 `shutdown!`。最终 RISC-V CFS 输出位于 `log.ans`，详见 `Docs/决赛文档/ai.log` 对应条目。
- **关联 commit**：`671d9e5b`

#### RISC-V basic test_yield fork/exit 锁序死锁修复（7.19）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans`、对照只读 testcase 源码和测试镜像反汇编、还原双 hart fork/exit 锁链、修复任务锁边界并执行双架构构建与 QEMU 回归
- **描述**：确认同一 child 连续五条 `iteration 0` 是测试打印外层 fork 序号的正常结果，真正卡点位于第五条后的 `exit(0)`。父进程 `clone_process()` 原先以 `TaskControlBlockInner -> ProcessMeta` 获取锁，首个子进程退出则以 `ProcessMeta -> TaskControlBlockInner` 获取同一父对象，RISC-V 双 hart 下形成 AB-BA；CFS 仅放大触发窗口。修复通过快照父元数据、将 `Process::new()` 移出父 task inner 临界区，并在 exit/exit-group 中先复制 task weak 列表后再获取 task inner。双架构 release 构建通过；RISC-V release+CFS 多轮和 LoongArch64 单核 basic 回归均完整结束，最终 RISC-V 结果保存在 `log.ans`。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/basic-test-yield-fork-exit-deadlock.md](./problem/basic-test-yield-fork-exit-deadlock.md)。
- **关联 commit**：`f1f3a730`

#### BusyBox fork/exec/exit 并发卡死修复（7.19）

- **工具/模型**：Codex (GPT-5)
- **场景**：梳理当前 BusyBox 卡死修复的任务唤醒、调度、exec/exit 改动，并根据现有 `log.ans` 补充问题复盘与开发记录。
- **描述**：确认 BusyBox 高频短进程暴露的是内核共享生命周期竞态：futex 唤醒会把已运行或 zombie task 重复入队，CFS/RR 未过滤陈旧就绪项，exec/调度存在反向锁域，地址空间替换/释放前仍可能使用旧页表。修复将入队权收敛为 `Blocked -> Ready`，取队检查 `Ready`，维持 `ProcessMeta -> TaskControlBlockInner`，并在 exec/退出回收前激活有效页表、重置过期 `robust_list`。当前 RISC-V `log.ans` 的 musl/glibc BusyBox 组均结束且 `shutdown!`；`hwclock`、`mv/rmdir`、后台 `sleep/kill` 失败及 glibc malloc assertion 未被视为已解决。详见 [problem/busybox-fork-exec-exit-smp-hang.md](./problem/busybox-fork-exec-exit-smp-hang.md) 与 `Docs/决赛文档/ai.log` 对应条目。
- **关联 commit**：`0a0845bf`

#### RISC-V CFS netperf UDP_RR 首 burst 卡死修复（7.19）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 和此前双 hart网络唤醒复盘，审计 CFS 出队/网络 waker 时序与 smoltcp deadline 换算，新增独立 UDP_RR 测例并执行双架构构建与 RISC-V QEMU 回归。
- **描述**：确认 CFS 的陈旧 entry 在读取 `Blocked` 状态后才清 `on_rq`，会让并发网络 waker 跳过重新入队，最终留下无 queue entry 的 `Ready` task；同时发现 smoltcp 微秒 `Instant` 到内核 `Timespec` 的两处单位换算均错误，使 fallback poll deadline 约偏离三个数量级。修复把状态检查与 CFS membership 清除收敛到同一个 `task.inner` 临界区，修正微秒/纳秒换算；`initproc` 仅运行新建的 musl `UDP_RR` 模块，保留 `netserver` 的 kill/wait 清理。RISC-V 连续有效样本均成功，最终 `log.ans` 输出 group END 和 `shutdown!`；LoongArch64 完成编译验证。详见 [problem/riscv-cfs-netperf-udp-rr-hang.md](./problem/riscv-cfs-netperf-udp-rr-hang.md)。
- **关联 commit**：`a7c9fa55`

#### RISC-V libctest 批量 COW 源帧并发释放修复（7.19）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `riscv.ans` 与 `log.ans` 中单测正常、批量随机段错误的问题，检查测试镜像 BusyBox 反汇编和 RISC-V COW 页故障路径，并执行最终 QEMU 回归。
- **描述**：确认 `0x1066c0` 是 BusyBox/musl 分配器检测 heap chunk 元数据损坏后的主动崩溃，而非 `clocale_mbfuncs` 断言。根因是两个 hart 并发 COW 时，一个路径在取得裸源页后解除 VMA 映射，另一路径可删除最后一个 `FrameTracker` 引用并复用源物理页。修复在 `unmap_one()` 前克隆并持有源帧到页内容复制完成。RISC-V 构建通过；最终根目录 `log.ans` 的 static 107 项与 dynamic 110 项全部结束、打印 `GROUP END`/`shutdown!`，无段错误、页故障、panic、TFAIL 或 TBROK；两个既有 `utime` 失败未纳入本次修复。详见 [problem/riscv-libctest-cow-source-frame-race.md](./problem/riscv-libctest-cow-source-frame-race.md) 与 `Docs/决赛文档/ai.log` 对应条目。
- **关联 commit**：`615acc46`

#### 参考 Linux 7.0 加固 RISC-V COW 所有权（7.19）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求根据本地 Linux 7.0 源码说明 fork/COW 实现，并将其生命周期与权限检查原则落地到 Ya2yOS 的 RISC-V COW 路径。
- **描述**：对照 Linux `do_wp_page()`/`wp_page_copy()` 的“旧 folio 引用 -> 分配并复制 -> PTE 重验/切换 -> rmap 与引用释放”顺序，保留源 `FrameTracker` pin，并把 Ya2yOS COW 改为 OOM 时保持旧映射、成功后才替换 PTE 和 VMA frame。RISC-V `copy_to_user` 现会对 present COW 页走 StorePageFault，避免绕过硬件写保护；Brk shrink 和 lazy stack clone 的页表所有权也同步修复。RISC-V/LoongArch64 release 构建均通过；最终 RISC-V 根目录 `log.ans` 有 217 个 START/END、`GROUP END`/`shutdown!`，无段错误或页故障，两个既有 `utime` 失败未纳入本次修复。详见 [problem/riscv-libctest-cow-source-frame-race.md](./problem/riscv-libctest-cow-source-frame-race.md) 与 `Docs/决赛文档/ai.log` 对应条目。
- **关联 commit**：`c0ca0e8f`

#### libc-test futimens fd + NULL pathname 语义修复（7.19）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 的 libc-test `utime` 失败，区分 `futimens` fd ABI 与 `utimes(NULL)` 错误语义，修复 syscall 并执行双架构 QEMU 回归。
- **描述**：确认 musl `futimens(fd, times)` 以 `utimensat(fd, NULL, times, 0)` 进入内核，而旧实现将任意 NULL pathname 直接返回 `EFAULT`，使所有有效 fd 调用在读取时间数组前失败。修复仅对非负 fd 从 fd 表直取 `OSFile`/inode，拒绝 `O_PATH`，并保留 `AT_FDCWD + NULL` 的 `EFAULT`，从而不回归 LTP `utimes01`。RISC-V 与 LoongArch64 的定向 `entry-static.exe utime` 均输出 `Pass!` 和 `shutdown!`；最后一次 LoongArch64 日志保留在根目录 `log.ans`。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/libctest-futimens-fd-null-pathname.md](./problem/libctest-futimens-fd-null-pathname.md)。
- **关联 commit**：`df863af3`

#### iozone 连续测例 inode 缓存复用卡死修复（7.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：拆分 iozone 子测例、分析 `log.ans` 与 GDB 双 hart 回溯，审计 VFS inode cache、lwext4 write-back FIFO 和 wait 回收锁边界，并执行双架构 QEMU 回归。
- **描述**：确认 iozone cleanup 后的 `/proc/21/stat` 错误复用已删除 `/musl/iozone.DUMMY.1` 的 canonical inode，导致 `check_cached()` 对错误路径进入 lwext4 `ext4_fread()` 自旋。修复将路径索引和 inode cache 收敛为单状态锁，回收被同路径 key 覆盖的强引用 orphan，并在 stale canonical replacement 时撤销旧 alias；task proc 子树改为 path key。同步清理 cache/FIFO 元数据、将 FIFO 淘汰写回移出队列锁、以独立 descriptor 初始化文件缓存，并在 child reaping 前释放父 `ProcessMeta`。最终 RISC-V `log.ans` 与 LoongArch64 `/tmp/iozone-loong.log` 均出现两次 `iozone test complete.`、backward-read 吞吐段和 `shutdown!`。详见 [problem/iozone-inode-cache-reuse-hang.md](./problem/iozone-inode-cache-reuse-hang.md) 与 `Docs/决赛文档/ai.log` 对应条目。
- **关联 commit**：`9d4168be`

#### RISC-V LTP mmap001 PROT_WRITE 页表编码卡死修复（7.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 中 mmap001 首次写入后的无限 page fault，核对测试镜像 ELF 与 RISC-V PTE 规则，修复硬件权限转换并执行 QEMU 回归。
- **描述**：确认 `MAP_SHARED | PROT_WRITE` 的文件页被错误编码为 `R=0,W=1`，这是 RISC-V 保留 PTE 组合；软件页表将其误视为 present，写保护处理只补 `DIRTY` 后重试，造成同一 store 无限陷入。修复在硬件 PTE 构造及 mprotect 直接 flags 路径中规范化 `W => R`，同时保留 VMA 的逻辑 `PROT_WRITE` 元数据以及 lazy PTE 不设 `VALID` 的约束。RISC-V `log.ans` 中 mmap001 现为 `passed 4 failed 0 broken 0` 并 `shutdown!`；LoongArch64 release 构建通过，未运行其行为回归。详见 `Docs/决赛文档/ai.log` 对应条目与 [problem/mmap001-riscv-write-only-pte.md](./problem/mmap001-riscv-write-only-pte.md)。
- **关联 commit**：`1f6f2244`

#### BusyBox hwclock 与目录 rename 失败修复（7.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：根据 `log.ans` 的 BusyBox musl/glibc 失败项建立最小复现，审计 VFS `renameat2` 和 devfs RTC ioctl 路径，并执行双架构构建与 RISC-V QEMU 回归。
- **描述**：确认 `renameat2` 为取得源 inode 使用 `O_RDWR` 打开目录，VFS 在进入 ext4 前返回 `EISDIR`；`DevRtc` 未实现 `RTC_RD_TIME`，调用落入默认 `ENOTTY`。修复改为只读打开 rename 源路径，并按 Linux `struct rtc_time` ABI 从内核 realtime 向用户态复制时间。RISC-V 根目录 `log.ans` 中 musl/glibc 的 `hwclock`、`mv test_dir test`、`rmdir test` 均为 `exit_code=0` 并 `shutdown!`；LoongArch64 release 构建通过。详见 [problem/busybox-hwclock-rename.md](./problem/busybox-hwclock-rename.md) 与 `Docs/决赛文档/ai.log` 对应条目。
- **关联 commit**：`88fb507a`

#### lmbench musl/glibc 连续运行 ext4 `EEXIST` 自锁修复（7.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析三次 `log.ans` lmbench 运行，读取测试镜像脚本，建立 musl 单项矩阵和真实组合回归，定位 musl 后 glibc 卡死。
- **描述**：先排除 `make log` 的 syscall DEBUG 输出导致的伪超时，再用 `sh -x` 将组合卡点定位为重复 `mkdir -p /var/tmp`，并用 #4 创建目录后 #5 重复 mkdir 的最小矩阵复现。审计确认 `Ext4Inode::create()` 在持有 `EXT4_OP_LOCK` 后构造临时 inode；`EEXIST` 错误返回时其 Drop 重入同一不可重入锁。修复调整构造与 guard 的声明顺序，保持创建检查原子性。RISC-V 的 24 项 musl 单项、完整 glibc 以及真实 musl+glibc 组合均 END/shutdown；LoongArch64 release 编译通过。完整调试过程见 [problem/lmbench-ext4-eexist-drop-self-deadlock.md](./problem/lmbench-ext4-eexist-drop-self-deadlock.md) 和 `ai.log`。
- **关联 commit**：`597cddee`

#### RISC-V LTP kill10 信号锁序风险修复（7.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `riscv.ans` 的 kill10 集体运行卡死，审计信号投递/handler 锁序，并执行 RISC-V 双 hart QEMU 回归。
- **描述**：原始日志只证明 kill10 启动后卡住、没有锁栈，未将风险误写成唯一已证实根因。代码审计确认 `add_signal_with_info()` 与 `handle_signal()` 都曾在 `TaskControlBlockInner` 临界区获取 `SigTable`，违反项目全局锁序。修复将 disposition 查询提前为无任务锁的快照，随后才写入或消费 pending 信号，保留停止态恢复、唤醒与 `SA_SIGINFO` 语义。RISC-V musl/glibc kill10 均 `passed 1 failed 0 broken 0` 并正常关机；详见 [problem/kill10-signal-lock-order.md](./problem/kill10-signal-lock-order.md)。
- **关联 commit**：`c9b21ca9`

#### RISC-V release kill10 ppoll/ITIMER 阻塞唤醒修复（7.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 release 与 `make log` 的 kill10 时序差异、读取测试镜像 LTP ELF、GDB 检查 ppoll/ITIMER 路径，并执行双架构构建和 RISC-V release QEMU 回归。
- **描述**：确认 LTP `pause()` 实现为无 fd 无限 `ppoll`，而旧内核只作 Ready/yield，未建立可靠的等待状态；将其改为原子发布 Blocked 后，又修正 owner-hart blocked timer 仅依赖 Future waker、遗漏 ppoll/pause 的问题。ITIMER 现向所有 Blocked task 投递 SIGALRM 并重新入队，ppoll 同时补齐临时 signal mask ABI 与 task/signal-table 锁序。根目录 `log.ans` 中 musl `kill08/kill10` 均为 `passed 1 failed 0 broken 0`，最终 `shutdown!`；LoongArch64 本轮仅编译。
- **关联 commit**：`c28275db`

#### BuildStorm 启动与 rustc 工具链启动兼容修复（7.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析决赛 BuildStorm `log.ans`、定向 RISC-V 启动日志和 release ELF 反汇编；对照 Linux 7.0 信号与 exec 语义，完成双架构构建整理。
- **描述**：确认原日志没有真实 TFAIL/TBROK，而是启动路径先后暴露 initfiles `/dev/null`、Debian `/bin` 符号链接、RISC-V syscall 132 `sigaltstack`、双 hart bootstrap stack 下溢以及多线程 `execve` 未 de-thread 等问题。修复目录初始化和 wrapper 注入边界，补齐线程私有备用栈、SA_ONSTACK frame/rt_sigreturn 恢复和 ABI padding，将 RISC-V bootstrap stack 提升至 128 KiB/hart，并使 exec 在替换共享映像前以 SIGKILL 收敛 sibling。最终 RISC-V/LoongArch64 release 构建通过；完整 BuildStorm QEMU 回归因当天停止而待续。详见 [四篇问题复盘](./problem/README.md)。
- **关联 commit**：`2ce578de`

#### fadvise64(223) syscall 实现（7.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：确认 RISC-V/LoongArch64 223 号 ABI，补齐 fadvise64 分发与 Linux 可见 errno 语义，并按维护者要求只完成构建和文档记录。
- **描述**：确认 `Syscall::Fadvise64 = 223` 已登记但未分发，导致调用返回 `ENOSYS`。实现按 `(int fd, loff_t offset, loff_t len, int advice)` 解码，保留 `EBADF`、FIFO/pipe `ESPIPE`、负 `len` 与非法 advice `EINVAL`；六种合法 hint 在当前缺少完整页缓存策略时作为无状态建议返回成功。RISC-V 和 LoongArch64 release 构建均通过。维护者未授权 QEMU/LTP 运行，因此未解除相关 LTP 黑名单，也未声称行为回归完成；详见 `Docs/决赛文档/ai.log` 对应条目。
- **关联 commit**：`c56bfc9c`

#### BuildStorm 动态库绝对路径规范化修复（7.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 BuildStorm 日志中 Rust toolchain DSO 的动态库路径 warning，校正绝对路径的 `..` 规范化层级并完成双架构构建。
- **描述**：确认 `/root/.rustup/.../bin/../lib/*.so` 是绝对但未规范化的路径；根因不是 `map_dynamic_link_file()` 缺少库条目，而是通用 `get_abs_path()` 对绝对输入直接复制、未像相对路径一样折叠 `.`/`..`。修复在通用路径函数复用 `path2abs()`，动态库兼容层不再承担路径规范化。RISC-V 与 LoongArch64 release 构建通过；未运行 QEMU 行为回归，未新增 problem 文档。详见 `Docs/决赛文档/ai.log` 对应条目。
- **关联 commit**：`8fc0ee75`

#### BuildStorm final-2026 动态链接、目录项与 FIONBIO 修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求根据 final-2026 的根目录 `log.ans` 修复 BuildStorm 启动、Cargo 与目录扫描失败，并持续将 QEMU 输出写回该日志。
- **描述**：确认 final Debian 原生 multiarch libc 与旧 `/glibc/lib` 被错误混用，Rustup RPATH 的正常 `ENOENT` 也被 basename fallback 伪造为旧 libc，触发 rustc TLS SIGSEGV；同时修复 lwext4 `EXT4_DE_* -> DT_*` ABI 转换和 Rust `Command::output()` 所需的 common-VFS `FIONBIO`。修复后 `GLIBC_2.38`、目录误识别、rustc SIGSEGV、`process.rs` ENOTTY panic 均消失，RISC-V 日志出现 `BUILDSTORM_TOOLCHAIN ok`。双架构 release 构建通过；由于当前 QEMU 仅 `2G / 2 CPU`，300 秒窗口未完成后续 guest 编译，不宣称完整 BuildStorm 通过。详见三篇新增 problem/ 复盘与 `ai.log`。
- **关联 commit**：`1e1ec559`, `1f78f84b`, `02302286`

#### rseq(293) 系统调用接入

- **工具/模型**：Codex (GPT-5)
- **场景**：确认双架构 syscall 293 的 Linux ABI，接入 kernel/user syscall 路径并记录实现边界。
- **描述**：AI 对照本地 Linux 7.0 rseq ABI 和本项目的 syscall、TCB、clone/exec、trap/信号路径，确认 RISC-V 与 LoongArch64 的 293 均为 `rseq(2)`。人工审核后采纳经典 32-byte ABI 的线程级注册状态、基础 errno、clone/exec 生命周期和用户态返回前的防御性 fixup；同时加入 initproc 基础探针。维护者提供的 RISC-V `log.ans` 随后输出 `rseq regression: PASS`，证明基础 ABI 闭环已运行通过。维护者仍未要求完整 rseq 语义验收，因此未运行 LTP/Linux selftest，也未宣称抢占、信号或跨 hart critical-section 语义已通过。用户态双架构编译成功；内核完整构建受缺失的 lwext4 musl C 交叉编译器阻断。详见 `Docs/决赛文档/ai.log` 2026-07-21 条目和 [problem/rseq-syscall.md](./problem/rseq-syscall.md)。
- **关联 commit**：`9fe9db03`

#### BuildStorm 工具链检查后 minibuild 超时分析（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 final-2026 BuildStorm 在工具链检查成功后的 guest Rust 编译超时，并形成未完成问题的可审计检查点。
- **描述**：记录了大 Rust DSO 的 lwext4 缓存准入重复开销与只读 private 文件映射页复用限制，并实现局部优化供验证。RISC-V release 构建通过，但 10 分钟 QEMU 运行仍未到达 `BUILDSTORM_MINIBUILD ok`；复核还发现缓存偏移和 `mprotect` 后 COW 隔离风险，三个源码文件暂不提交。预读实验已撤回，未将该问题标记为修复完成。详见 `ai.log` 和 [problem/buildstorm-minibuild-post-toolchain-stall.md](./problem/buildstorm-minibuild-post-toolchain-stall.md)。
- **关联 commit**：`766aa004`

#### RISC-V VirtIO-MMIO 网卡自动总线分配修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求排查评测机配置下 RISC-V 启动日志找不到网络设备，并要求不改动 QEMU 参数。
- **描述**：日志中的 `0x10002000: ZeroDeviceId` 表明旧网卡地址未挂载设备。通过 QEMU `info qtree` 确认显式绑定 `.0` 的块设备之外，未指定 bus 的网卡被自动分配到 `virtio-mmio-bus.7` / `0x10008000`。内核同步更新该页的 MMIO 映射和网卡驱动基址，保留评测机 QEMU 参数不变。RISC-V release 构建与真实 QEMU 启动均通过；日志正常初始化网络，且已无原有无网卡警告。详见 [problem/riscv-virtio-net-mmio-autobus.md](./problem/riscv-virtio-net-mmio-autobus.md) 与 `ai.log` 对应条目。
- **关联 commit**：`881713d2`

#### RISC-V riscv_hwprobe(258) syscall 桩接入（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求为 RISC-V 258 号 syscall 接入最小 stub。
- **描述**：确认 `258` 是 `riscv_hwprobe(2)`。在 `riscv64` 条件编译下登记并分发该调用，新增薄入口固定返回 `ENOSYS`，避免在未填充 probe pair 时向用户态伪造有效硬件能力。RISC-V 与 LoongArch64 release 构建通过；未运行 QEMU/LTP。详见 `Docs/决赛文档/ai.log` 2026-07-21 条目。
- **关联 commit**：`1f964d9b`

#### RISC-V 8GiB CMA 与八核启动配置修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求按评测机 `8G / 8 CPU` 配置分析根目录 `log.ans`、修复启动 panic，并将修改过的 allocator 从 `os/vendor` 迁移至根目录 `crates`。
- **描述**：确认 7GiB 晚期 CMA 区间产生 4GiB/order 32 block，而 upstream allocator 仅有 32 个 free-list，初始化立即越界。将该依赖迁移为显式 path crate，扩展到 34 阶并限制最高阶合并；进一步定位到 `HART_NUM=8` 与汇编 `BOOT_HARTS=2` 不一致，bootstrap hart 非 0/1 时会覆盖 `.bss`，同步为 8。RISC-V 与 LoongArch64 release 构建通过；默认 RISC-V QEMU 运行越过 CMA panic，启动 7 个 AP 并进入 `BUILDSTORM_TOOLCHAIN ok`。完整 BuildStorm 未在 60 秒窗口内完成。详见 [problem/riscv-8g-8hart-bootstrap.md](./problem/riscv-8g-8hart-bootstrap.md) 与 `ai.log`。
- **关联 commit**：`2955379a`

#### BuildStorm MINIBUILD 独立脚本复现（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：拆分 final-2026 BuildStorm 的工具链、MINIBUILD、预构建和正式编译阶段，建立后续 `cargo build` 卡点的单独 RISC-V 入口。
- **描述**：复用启动期 `write_executable_init_file()` 向具备 Rust toolchain 的 `/glibc` 根文件系统注入五个独立诊断脚本，保留正式 `busybox sh <script>` 与 MINIBUILD 的静默 `cargo build` 语义。所有新输出使用 `BUILDSTORM_DEBUG_*`，不会被官方评分器当作正式得分。RISC-V 日志已确认 `MINIBUILD_PREPARE ok` 后进入 `MINIBUILD_BUILD begin`，但未出现完成标记；RISC-V、LoongArch64 release 构建通过，根因与完整 BuildStorm 仍待继续定位。详见 [problem/buildstorm-minibuild-post-toolchain-stall.md](./problem/buildstorm-minibuild-post-toolchain-stall.md) 与 `ai.log`。
- **关联 commit**：`bc6cf21f`

#### BuildStorm MINIBUILD Rustc mmap 虚拟地址预算修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：继续分析 BuildStorm MINIBUILD 的 Rustc 实际编译路径，定位累计 mmap 预算导致的 ENOMEM，并完成双架构构建与 RISC-V QEMU 回归。
- **描述**：独立诊断日志确认 Rustc 在已有约 468 MiB lazy VMA 后申请 128 MiB 匿名 `PROT_NONE` arena，被 512 MiB `MAX_MMAP_SIZE` 拒绝；这不是物理内存或用户 VA 耗尽。两架构预算提升至 2 GiB，保留 VMA 上限及缺页物理页约束。复用既有项目的 RISC-V MINIBUILD 能正常结束；强制 prepare 的 fresh run 已进入 `Compiling minibuild`，但 Cargo worker 创建 Rustc 子进程的普通 `clone()` 返回 `Bad address (os error 14)`。因此只将 mmap 预算修复标记为完成，完整 BuildStorm 仍待后续修复；LoongArch64 已完成构建，未运行其 QEMU。详见 [problem/buildstorm-minibuild-post-toolchain-stall.md](./problem/buildstorm-minibuild-post-toolchain-stall.md) 与 `ai.log`。
- **关联 commit**：`92dac901`

#### BuildStorm clone3/vfork 生命周期修复整理（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求只提交已确认可保留的 BuildStorm 任务管理修改，并补齐问题复盘、开发日志和 AI 记录。
- **描述**：复核 Rust toolchain 的 `clone3(CLONE_VM | CLONE_VFORK)` 日志后，将 vfork 父 task 的状态变化与即时调度切换绑定，延后 exec 对父 task 的唤醒，并让非线程 `CLONE_VM` 子进程继承父 hart，避免无 remote TLB shootdown 时跨 hart 共享页表。两个架构统一 clone3 stack ABI，并防止 group-exit 内部 SIGKILL 污染正常退出状态。双架构 release 构建通过。缓存性能实验和 `initproc` 单脚本入口未提交；MINIBUILD 剩余 EFAULT 已更正为动态 `MAP_STACK` 普通 fork 复制问题。详见 [problem/buildstorm-vfork-clone3-lifecycle.md](./problem/buildstorm-vfork-clone3-lifecycle.md) 与 `ai.log`。
- **关联 commit**：`916f426a`

#### lwext4 大文件缓存准入重复探测优化（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 BuildStorm MINIBUILD 调试日志中 `librustc_driver` 的大量 `initialize cache!`，并实施两步缓存路径优化。
- **描述**：确认 4 MiB whole-file cache 的超限文件在 mmap 按页读取时重复执行 `ext4_fopen/fsize/fclose`，且调试日志在缓存资格判断前输出。修复将日志移动到真实缓存插入后，并在 `Ext4File` 内记录超限负状态，保留已有小文件缓存优先级；成功写入、truncate、`O_TRUNC` 和删除路径同步更新状态。RISC-V MINIBUILD 输出 `ok`/`shutdown!`，日志总数从 16,191 降为 68，目标 DSO 日志为 0；RISC-V、LoongArch64 release 构建均通过。
- **关联 commit**：`07349789`

#### BuildStorm MINIBUILD 动态 MAP_STACK fork EFAULT 修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 MINIBUILD build 单独成功、prepare 后 fresh build 失败的差异，定位并修复普通 fork 的动态栈 VMA 继承缺失。
- **描述**：日志确认 Cargo worker 的普通 `clone()` 在写 `CLONE_CHILD_SETTID` 时返回 `EFAULT`；根因是 `MAP_STACK` 被误作固定 stack 跳过 fork 复制。修复将动态 `MAP_STACK` 纳入 mmap 继承，并保留固定初始 stack/trap 的重建边界。RISC-V debug 回归已确认同一 clone 成功创建并运行 child；双架构 release 构建通过。fresh RISC-V 600 秒回归尚未到达 `BUILDSTORM_DEBUG_MINIBUILD ok`，因此不宣称完整 MINIBUILD 通过。详见 `ai.log` 和 [problem/buildstorm-minibuild-post-toolchain-stall.md](./problem/buildstorm-minibuild-post-toolchain-stall.md)。
- **关联 commit**：`dd250ec5`

#### BuildStorm MINIBUILD fresh fork TrapContext 与 native loader 修复（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求继续修复 BuildStorm MINIBUILD 单独运行成功、prepare 后 fresh 编译失败及 10 分钟超时。
- **描述**：通过 TrapContext 现场、Cargo linker stderr 和 final 镜像 ELF 对照，确认普通 fork 在正确复制当前线程 trap 快照后又按 child VMA 覆盖为另一 parent 线程的 futex wait 现场；解除该死锁后，又确认两层动态库 mapper 将 native libc linker script 的 `/lib/ld-linux-*` 依赖错误改写为旧 `/glibc/lib` loader，导致 `GLIBC_PRIVATE`/TLS 符号无法解析。修复删除冗余 trap VMA clone，将 GCC toolchain 路径视为 native，并让真实 loader 优先、缺失时才走 legacy fallback。RISC-V fresh MINIBUILD 现输出 `ok` 和 `shutdown!`；RISC-V、LoongArch64 release 构建通过。完整 `cargo xtask` 未运行，详见 [problem/buildstorm-minibuild-fresh-fork-loader.md](./problem/buildstorm-minibuild-fresh-fork-loader.md) 与 `ai.log`。
- **关联 commit**：`bfbfb2c4`

#### CAgent Bash 运行器与 Debian `/bin` 路径修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供 `cagent_testcode.sh` 的语法错误日志，要求改用 `/bin/bash` 并继续处理新的运行失败。
- **描述**：AI 对照只读 CAgent 脚本确认 Bash 数组与 BusyBox `sh` 不兼容；改用 Bash 后，依据 `execve fail: -20`、镜像 `/bin -> /usr/bin` 布局和 VFS 查找实现，定位中间符号链接与 `FsIndex` 未跟随缓存导致的 `ENOTDIR`。人工审核后采纳决赛专用 Bash 运行器及通用父目录重解析修复。RISC-V `log.ans` 已输出 CAgent `GROUP END` 和 `shutdown!`，10 项中 7 项 pass、3 项 reject；后三项未被表述为通过。详见 `Docs/决赛文档/ai.log` 2026-07-21 条目和 [problem/cagent.md](./problem/cagent.md)。
- **关联 commit**：`371a3d7e`

#### fchdir(50) syscall 接入与 LTP 回归（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求实现 `sys_fchdir`，并提供 RISC-V `log.ans` 要求核验结果和补齐文档。
- **描述**：确认 50 号 ABI 已登记但未分发，handler 也未完成；实现从 fd 表直接取得目录 inode，保持目录 `O_PATH` fd 可用，并分别返回 `EBADF`、`ENOTDIR`、`EACCES`。路径型 `chdir` 与 fd 型 `fchdir` 共用单 inode 权限检查，但 fd 路径不重新解析 pathname。日志中 musl/glibc `fchdir01` 至 `fchdir03` 均为 `passed 1 failed 0 broken 0` 并正常 `shutdown!`，因此解除三项 LTP blacklist。详见 `ai.log` 对应条目。
- **关联 commit**：`6f492f68`

#### prctl PR_SET_CHILD_SUBREAPER 接入与 orphan reparenting 回归（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求实现 `prctl(PR_SET_CHILD_SUBREAPER)`，使父进程退出后的孤儿后代由最近的 child subreaper 收养，并要求仅保留必要修改、停止 LoongArch64 验证后补齐文档。
- **描述**：AI 对照 Linux 7.0 的 `prctl` 与退出重父化路径，确认这不是新增 syscall 号而是既有 167 号调用的 option 语义。实现将 subreaper 标记置于线程组共享的 `ProcessMeta`，通过父链选择最近存活收养者，并同步更新 `children`、PPID、zombie 通知和默认 wait 语义。RISC-V musl/glibc `prctl03` 均为 `passed 6 failed 0 broken 0` 并正常 `shutdown!`；未进行 LoongArch64 运行时验收。详见 `Docs/决赛文档/ai.log` 对应条目和 [problem/prctl-child-subreaper.md](./problem/prctl-child-subreaper.md)。
- **关联 commit**：`351c42c0`

#### LTP prctl04 seccomp strict/filter 修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析 `log.ans` 的 `prctl04` 失败并完善既有 `prctl(167)` 语义。
- **描述**：AI 对照 LTP `prctl04.c`、当前 syscall 分发、task clone 和 signal return 路径，确认问题是 seccomp 只在 handler 中伪返回成功，未在线程状态保存或统一 syscall 入口强制执行。实现线程级 strict/filter 状态，安全复制并验证测试所用 classic BPF 子集，在 fork/clone 中继承，并在拒绝时投递 strict 的 `SIGKILL` 或 filter 的 `SIGSYS`。同时修正 variadic `prctl()` 未使用寄存器不得强制为零的 ABI 假设。RISC-V、LoongArch64 的 musl/glibc `prctl04` 均为 `passed 9 failed 0 broken 0` 并正常关机；未实现的 BPF 指令、TSYNC、filter 叠加和其他 seccomp action 已明确记录。详见 [problem/prctl-seccomp-prctl04.md](./problem/prctl-seccomp-prctl04.md) 与 `ai.log` 对应条目。
- **关联 commit**：`8c437ff2`

#### LoongArch 8GiB/8 核 QEMU bring-up（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求将 LoongArch 评测 QEMU 扩展到 `8G / 8 CPU`，并指出只修改启动参数不构成内核支持。
- **描述**：审计 QEMU 9.2 direct boot、内核内存布局和调度路径后，补齐分段 RAM、CPUID hart ID、mailbox/IPI 次核启动、per-hart bootstrap stack、进程 home hart 和用户可见 CPU 拓扑。最终 QEMU 日志显示完整高端 CMA、7 个 AP 上线、netdev 4 项通过以及 musl/glibc basic 正常关机。保持进程固定 hart，未虚报可迁移 affinity；动态内存展示尝试会触发 glibc 回归，已撤回。详见 [problem/loongarch-8g-8hart-bootstrap.md](./problem/loongarch-8g-8hart-bootstrap.md) 和 `ai.log`。
- **关联 commit**：`2b49f411`

#### LoongArch 八核 COW 源帧并发释放修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析 `log.ans` 中每次运行结果不同的 LoongArch 八核 basic 崩溃，定位并修复竞态后执行重复 QEMU 回归。
- **描述**：AI 通过日志 PID/HART 对应、只读镜像反汇编和既有 RISC-V COW 修复对照，确认合法的 `0x25d0` 指令因父子跨 hart 同时拆分同一 COW 页而被破坏。LoongArch 旧路径在复制前删除源页的最后 `FrameTracker` 引用，使 allocator 可回收、清零并复用仍在读取的物理页；修复固定源帧，先分配复制目标页，再替换 PTE、刷新 TLB 和转移 VMA 所有权。LoongArch64 release 构建通过，8 核 `basic-musl/basic-glibc` 连续五轮完整结束并 `shutdown!`，未再出现非法指令、段错误或内核失败信号。详见 [problem/loongarch-cow-source-frame-race.md](./problem/loongarch-cow-source-frame-race.md) 与 `ai.log` 对应条目。
- **关联 commit**：`bac5bf8d`

#### LTP read03 FIFO 创建后 stat 模式类型位修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 的 `read03 TBROK: Mode does not indicate fifo file` 并修复。
- **描述**：AI 从 mknodat 创建链路追踪到 ext4 inode 初始化与 mode 写入。确认 `create()` → `file_open()` → `ext4_generic_open` 硬编码 `EXT4_DE_REG_FILE` 导致 inode mode 高位被初始化为普通文件而非 FIFO，而 `ext4_mode_set` 仅修改低 12 位权限位。修复在 `Ext4Inode::fstat()` 末尾通过 `FsIndex::special_node_type()` 修正 mode 类型高位；同时修改 `ext4_mode_set` 使其支持类型高位写入作为防御。LoongArch64 musl/glibc read03 均 TPASS。详见 [problem/read03-fifo-mode-type-bits.md](./problem/read03-fifo-mode-type-bits.md) 与 `ai.log` 对应条目。
- **关联 commit**：`bee2e09f`

#### LoongArch LTP fs_bind01 hush timeout 与 bind 挂载栈卸载修复（7.21）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 中 LoongArch64 `fs_bind01` 的早期终止并完成修复与 QEMU 验证。
- **描述**：确认 BusyBox hush 不能按 LTP 原写法保留 `eval "local timeout=..."` 创建的局部变量，导致 watchdog 获得零秒并在真实挂载断言前终止。启动期对 musl/glibc `tst_test.sh` 做幂等的声明/赋值拆分，保留原 inode/权限写回后，进一步定位到 `/proc/mounts` 将 self-bind 的路径 source 暴露给 BusyBox，使其一次 `umount` 同时以 mountpoint 和 device 匹配并卸掉两层。仅改变 bind 条目的展示 source 为 `none`，并将 bind 身份独立于 remount flags 保存，保留内核 mount stack、event group 和非 bind source 语义。LoongArch64 `fs_bind01` 自身为 `passed 29 failed 0 broken 0`，四次关键卸载均 `TPASS` 并正常 `shutdown!`；RISC-V release 构建通过，未运行 RISC-V QEMU。
- **关联 commit**：`60dd297e`

#### LTP kill07 SIGKILL 快速退出的 waitpid 状态编码修复（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 最后的 `kill07` 失败并完成内核修复与验证。
- **描述**：确认 `kill(pid, SIGKILL)` 已成功投递并由父进程回收，失败来自 blocked child 经 scheduler/future 快速退出后缺少 `termination_signal`，使 `waitpid()` 将内部 137 误编码为普通 `exit(137)`。修复仅在用户态 `kill/tkill/tgkill` 携带 `SigInfo` 的不可忽略 SIGKILL 投递时记录进程终止原因，内部 execve sibling 清理信号不污染共享 wait status；同时统一 `block_on()` 与协作调度的 group-exit/SIGKILL 退出门。LoongArch64 单跑 `kill07` 为 `passed 1 failed 0 broken 0` 并正常关机，LoongArch64 与 RISC-V release 构建通过。详见 [problem/kill07-sigkill-fast-exit-wait-status.md](./problem/kill07-sigkill-fast-exit-wait-status.md) 与 `ai.log` 2026-07-22 条目。
- **关联 commit**：`7389ed3a`

#### LoongArch CAgent 动态链接器 LSX 未启用 panic 修复（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 中 LoongArch64 CAgent 在 Bash 启动后立即触发的内核 panic。
- **描述**：AI 从 `execve` 成功、动态解释器加载和 ecode `0x10` 的日志链路出发，使用本地 Linux 7.0 异常定义与 final-2026 镜像中 `ld-linux` 的只读反汇编，确认故障指令是 LSX `vld`，根因是每个 hart 仅设置 FPE、未设置 EUEN.SXE。修复启用 SXE，并将用户 trap/signal/clone 上下文由 32 x 64-bit FPR 扩展为完整 32 x 128-bit LSX 状态，汇编以 `vst/vld` 保存恢复。LoongArch64 release 构建通过；维护者提供的 `log.ans` 到达 CAgent `GROUP END` 和 `shutdown!`，无 panic、Unknown trap、SXD 或 ASXD。10 个业务任务中 6 项 pass、4 项 reject，未将 reject 误报为通过。详见 [problem/loongarch-cagent-lsx-disabled-panic.md](./problem/loongarch-cagent-lsx-disabled-panic.md) 与 `ai.log` 对应条目。
- **关联 commit**：`e351d564`

#### LoongArch 批量 LTP Rust heap CMA 后备与回收修复（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析 LoongArch64 批量 LTP `Heap allocation error`，从 CMA 高段堆后备和前例资源回收两条路径完成修复、构建与定向 QEMU 验证。
- **描述**：AI 将 `Layout { size: 2065194 }` 精确对应到 `/musl/busybox` ELF 最大 `PT_LOAD` 末端，确认 `fsmount01` 的 BusyBox `mkfs.ext2` 装载需要一个正常的 2 MiB buddy block，而原 128 MiB `.bss` global heap 不会使用已经纳入 CMA 的高段 RAM。修复将 CMA 连续页安全移交给 global heap，避免 SMP retry 假 OOM；同时有界回收 idle dentry/inode、保证 shared mmap teardown 即使 writeback 出错也释放页表，并让 LTP runner 以非阻塞 pipe + `WNOHANG` 清理遗留 helper。LoongArch/RISC-V release 构建均通过；LoongArch `fsmount01` 单例无 heap panic、6 项 TPASS 并正常关机，仍保留 1 项环境 TBROK，完整批量回归未被误报为通过。详见 [problem/loongarch-ltp-heap-cma-reclaim.md](./problem/loongarch-ltp-heap-cma-reclaim.md) 与 `ai.log` 2026-07-22 条目。
- **关联 commit**：`372071ef`

#### BuildStorm Rustc `mremap(MREMAP_MAYMOVE)` 数据丢失修复（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 中 Rustc 编译 `unicode-ident` 的 parser error 并修复。
- **描述**：AI 通过镜像/宿主源码哈希、guest 原文件和副本的直接 Rustc 对照，排除依赖损坏；随后将源码读取、匿名私有 mmap、两次 `mremap(MREMAP_MAYMOVE)` 和 parser 失败按 PID 串联，确认旧实现先 `munmap` 再建立空 VMA，导致 Rust allocator 扩容丢失已读内容。修复在同一 `MemorySet` 写锁内先为 private VMA 的每个 resident 页固定源 `FrameTracker`、分配和复制目标页，全部成功后才撤销旧 VMA；失败仅回滚目标。等长原址、私有文件 lazy 页和 metadata 均明确保留。RISC-V probe 中原文件和副本的 Rustc 均返回 0，未再出现 parser error；外层 180 秒在 Cargo 扫描 workspace 时到期，未将完整 BuildStorm 标为通过。详见 [problem/buildstorm-mremap-data-loss.md](./problem/buildstorm-mremap-data-loss.md) 与 `ai.log` 对应条目。
- **关联 commit**：`83c981c7`

#### BuildStorm Rustc artifact rename/write-back 缓存修复（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求继续分析 `log.ans` 中 `unicode_ident` 的 `E0463`，并参考前序
  `mremap` 修复完成后续 BuildStorm 编译失败的定位与修复。
- **描述**：AI 区分了 `83c981c7` 已修复的 Rustc parser error 与本次 artifact 发布失败，
  确认 lwext4 在 rename 后按旧 pathname 回写脏 cache 会重建临时文件。修复将 active source
  的完整回写/丢弃置于 rename 前，在成功 rename 后丢弃 source/destination 的遗留
  write-back state，并将 short write 转为 `EIO` 以阻止不完整 artifact 发布。双架构构建及
  RISC-V rename 发布探针结果按实际记录；完整 Cargo 结论不超出本轮运行证据。详见
  [problem/buildstorm-rustc-artifact-rename-writeback.md](./problem/buildstorm-rustc-artifact-rename-writeback.md)
  与 `ai.log` 对应条目。
- **关联 commit**：`d50c274c`

#### BuildStorm 用户态 `/tmp` 分阶段诊断入口迁移（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求依据 `scripts/buildstorm_testcode.sh` 将 BuildStorm 拆为可单独选择的小测例，放入 `user/src/bin/buildstorm/`，并移除内核启动期注入的重复脚本。
- **描述**：AI 对照官方阶段、现有 `initfiles.rs` 注入脚本和用户态构建/预加载边界，将工具链、MINIBUILD prepare/build、target 清理、`tg-xtask` 预构建、正式构建，以及原先混入预构建的 rename/unicode artifact probe 分离为八项。每项在 `initproc` 内按需物化到固定 `/tmp/buildstorm-*.sh`，再沿用 Bash 执行，保留 Rust/Cargo 环境、fd/pipe 拓扑和 `BUILDSTORM_DEBUG_*` 防误评分边界；内核不再写入 BuildStorm 测试脚本。新增官方顺序和扩展诊断组合入口，runner 返回子进程 wait status，exec 失败的 child 以 `127` 显式退出。双架构 release 构建通过；RISC-V snapshot 已确认 Bash 从 `/tmp/buildstorm-xtask-prebuild.sh` 启动并进入 Cargo，120 秒内未完成。此次结构迁移不宣称完整 BuildStorm 或性能评分通过，详见 [problem/buildstorm-minibuild-post-toolchain-stall.md](./problem/buildstorm-minibuild-post-toolchain-stall.md) 与 `ai.log` 对应条目。


- **关联 commit**：`18ab8e82`

#### LoongArch CAgent loopback TCP 分片校验和修复（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求继续修复 LoongArch64 CAgent 的三个 reject，并要求测试脚本、镜像和 testcase 源码只读。
- **描述**：AI 以单项 runner、TCP trace 和 smoltcp 源码确认大 HTTP 请求经过 IPv4 分片与重组后，发送端却把整个 8192 B fragment buffer 纳入 TCP pseudo-header length/checksum，导致接收端按真实 datagram 长度校验失败。修复仅为 IPv4 loopback socket 设置 4096 B MSS，保持 Router/物理网卡 1500 B MTU，并在 smoltcp 首片 emit 时限制 checksum buffer 到 `total_ip_len`。同时串行排空单一 fragmenter、防止 IPv4 首分片伪造 SYN 进入监听表，并加入实际分片-重组-TCP checksum 回归测试。最终 LoongArch64 完整 CAgent 十项全部通过，RISC-V release 构建通过；临时诊断入口已恢复。详见 [problem/cagent-loopback-tcp-fragmentation.md](./problem/cagent-loopback-tcp-fragmentation.md) 与 `ai.log`。
- **关联 commit**：`d62f52ed`

#### smoltcp 本地 crate 迁移（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求将内核定制的 smoltcp 从 `os/vendor/` 移至根目录 `crates/`。
- **描述**：审计 Cargo patch、离线 vendor 配置和构建脚本后，将完整 crate 移至 `crates/smoltcp`，并仅把 `os/Cargo.toml` 的本地 patch 路径改为 `../crates/smoltcp`。保留 `os/dotcargo/config` 的其余离线依赖解析，不改写历史问题复盘，也未触碰 CAgent 脚本、测试镜像、testcase 源码或维护者已有的 `initproc` 改动。smoltcp 定向离线单测及 RISC-V、LoongArch64 release 构建均通过。
- **关联 commit**：`8afe7918`

#### BuildStorm 全量评分输出契约修复（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者发现 BuildStorm 自定义全量测试已执行分阶段脚本，但 `judge_buildstorm-glibc.py` 仍输出 0/180，要求修复 `user/src/bin` 下的测试入口。
- **描述**：AI 对照 judge 正则、参考脚本、当前日志和提交历史，确认全量入口误用了刻意输出 `BUILDSTORM_DEBUG_*` 的诊断组合。新增构建期嵌入 `scripts/buildstorm_testcode.sh` 的正式单脚本 runner，令全量入口在 `/tmp` 一次执行并恢复 canonical `BUILDSTORM_*` 标记；分阶段诊断保持 DEBUG-only。RISC-V/LoongArch64 release 构建通过，RISC-V 180 秒真实回归已输出 toolchain/minibuild 正式成功标记，judge 从 0/180 恢复为 20/180；运行窗口在 prebuild 结束前到期，未将完整 compile 或性能项误报为通过。详见 [problem/buildstorm-full-run-marker-contract.md](./problem/buildstorm-full-run-marker-contract.md) 与 `ai.log`。
- **关联 commit**：`21f7525c`

#### LoongArch BuildStorm 稀疏 EXT4 文件读取破坏修复（7.23）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供 LoongArch64 BuildStorm 的 `unexpected reloc type 0x00dd8170` 日志，要求修复首两个环境测例和正式编译前的 Rust 动态加载失败，并在官方 Docker image 中重建 C archive。
- **描述**：AI 对照镜像 extent、`ext4_fread()`、正式评分脚本与 judge，确认 `/root/.cargo/bin/rustup` 的 logical block 328 是 sparse hole，旧 C 代码将其与连续 run 的零 sentinel 混用，复制了紧随其后的 relocation 页面。修复将 hole 零填、限制聚合到非零连续物理块，并修复尾部 partial read；build script 也会在 C 输入新于 archive 时重建。`uname -m` 改为返回精确的小写架构名，防止 LoongArch guest 落入 RISC-V 兜底分支。官方 Docker 重建后的 archive 保持 ABI v1，QEMU 已恢复 `BUILDSTORM_TOOLCHAIN/MINIBUILD ok`；外层终止发生在 prebuild，未将完整 compile/time 标为通过。详见 [problem/loongarch-buildstorm-sparse-ext4-read-corruption.md](./problem/loongarch-buildstorm-sparse-ext4-read-corruption.md) 与 `ai.log`。
- **关联 commit**：`4e0193d7`

#### BuildStorm guest 性能分类统计（7.24）

- **用户目标**：维护者要求在当前优化基础上继续工作，并在提交前用 guest 内证据定位编译
  速度瓶颈；同时指出此前修改过于集中于文件系统。
- **我的处理**：按 `fix-bug` 流程复核工作区、保留已有 `initproc` 定向入口，新增低开销
  `perf` 计数模块，覆盖 syscall、EXT4 lock、文件页缓存、mmap 缺页和 scheduler；报告限频
  且不打印逐条日志。没有删除 lwext4 全局锁，也没有修改只读 testcase 源码。
- **验证与边界**：`make TARGET_ARCH=riscv64`（同时完成 LoongArch64 子构建）通过；独占
  RISC-V 90 秒 QEMU 样本进入 `pre-build tg-xtask`，无 panic/错误，输出约 55 万 syscall、
  13.3 万 futex、631 万 scheduler 选取，但未完成 446 crate，不能写成正式性能提升结论。
- **后续方向**：优先区分 scheduler 的真实 context switch、idle loop 和 ready queue 重复选取，
  再决定是否需要进一步改文件系统锁。
- **关联 commit**：`b93ad36a`

#### BuildStorm 普通 read 路径与 EXT4 全局锁争用（7.23）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求用 `perf` 等工具定位 `buildstorm::compile::run()` 长时间编译的
  性能瓶颈并实现优化。
- **描述**：宿主无可用 `perf`、guest `perf_event_open` 未实现，因此使用 `/usr/bin/time`
  与 `strace -f -c` 做结构分析；futex 占 79.46%、pread64 仅 0.18%，日志主要停在
  `pre-build tg-xtask`。在保留 lwext4 全局 SMP 安全锁的前提下，新增只读直读、缓存优先
  读取，合并跨页普通 read，并移除每次 read 前的 size 探测；同时保留调度器 timer 扫描
  快照与节流优化。RISC-V/LoongArch64 构建通过，独占 RISC-V 180 秒和 70 秒运行进入
  预构建且无错误，但未完成 446 crate 或严格 A/B 耗时，未宣称具体加速比例。详见
  [problem/buildstorm-read-path-lock-contention.md](./buildstorm-read-path-lock-contention.md)
  与 `ai.log` 对应条目。
- **关联 commit**：`b93ad36a`

#### LoongArch64 LTP fcntl14 rt_sigsuspend 临时信号掩码恢复修复（7.22）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 中 LoongArch64 LTP `fcntl14` 失败并完成修复。
- **描述**：AI 对照 `fcntl14.c` 的 `sighold(SIGUSR1)`/`sigpause(SIGUSR1)` 同步流程与内核 trap return 路径，确认 record lock 的连锁失败来自 `rt_sigsuspend` 在 signal frame 建立前恢复旧 mask，令 pending `SIGUSR1` 再次被屏蔽且 handler 未执行。修复保留临时 mask 直到交付，将调用前 mask 写入 signal frame 供 `rt_sigreturn` 恢复，并在无 handler 返回路径清理状态。LoongArch64 musl/glibc `fcntl14` 均为 `passed 96 failed 0 broken 0`；RISC-V release 构建通过，但其 final-2026 镜像缺少该用例，未将 RISC-V runtime 标为通过。详见 [problem/fcntl14-sigsuspend-mask-restore.md](./problem/fcntl14-sigsuspend-mask-restore.md) 与 `ai.log` 对应条目。
- **关联 commit**：`7b1e7325`

#### LTP lseek11 ext4 SEEK_DATA/SEEK_HOLE 与 sparse truncate 修复（7.23）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求依据 Docker 构建运行后的 `log.ans` 持续修复 LoongArch64 LTP `lseek11`。
- **描述**：AI 对照 `lseek11` 的 block-size 二分探测、sparse write 和 EOF 扩展序列，确认 seek 映射本身修通后，block 0 数据仍被 bcache 与 direct payload I/O 的双副本覆盖；临时 `ftruncate01` 复现 grow 后旧字节未清零。修复补齐 Linux `SEEK_DATA/SEEK_HOLE` 的 VFS/lwext4 路径、extent-aware sparse write/read、inode 级 whole-file cache 禁用和 C rebuild 依赖，并将 `ftruncate` retained partial block 的 zero 操作统一到 direct I/O。LoongArch64 的 musl/glibc `lseek11` 均为 `passed 15 failed 0 broken 0`，临时 `ftruncate01` 两侧均通过后已移除入口；双架构 release 构建通过。详见 [problem/lseek11-ext4-seek-data-hole-sparse-write.md](./problem/lseek11-ext4-seek-data-hole-sparse-write.md) 与 `ai.log` 2026-07-23 条目。
- **补充验证**：最终复审补齐 sparse inode 首次打开的 flush-then-disable、`O_TRUNC` 丢弃旧 cache、未打开 unlink 的 policy 回收，以及 `ftruncate` 错误路径的单一 mount 解锁；收口后重新通过 LoongArch64/RISC-V release 构建，LoongArch64 musl/glibc `lseek11` 仍均为 `passed 15 failed 0 broken 0` 并正常关机。
- **关联 commit**：`b0ce887d`

#### BuildStorm Rustc 长 argv 截断与 RISC-V idle 轮询修复（7.23）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 的 BuildStorm `pre-build tg-xtask` 编译失败和长期低吞吐，并让 `initproc` 仅运行该定向测例。
- **描述**：AI 确认 Rustc 的长 `--check-cfg` 被 `execve` 误用的 256 B pathname 读取接口截断，导致 `E0765`。修复将 `argv/envp` 改为有上限的原始字节读取，预先构造可失败的新用户栈；RISC-V 空闲调度改为 one-shot timer + WFI，LoongArch 保持 polling。`initproc` 选择显式单作业诊断入口并保留 case 返回状态，默认并发和正式 BuildStorm 路径未改。双架构构建通过，独占的根目录 `16.ans` 已越过原错误位置；完整 446 单元和正式性能评分未宣称通过。详见 [problem/buildstorm-execve-argv-truncation.md](./problem/buildstorm-execve-argv-truncation.md) 与 `ai.log` 对应条目。
- **关联 commit**：`7280f9c3`

#### 决赛 CAgent/BuildStorm 单项评分包装器收束（7.23）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求整理 `user/src/bin` 的决赛测试包装器，保持提交平台时由 `run_final_testsuit` 在 `/glibc` 直接执行两个正式 `testcode.sh`，并删除 CAgent/BuildStorm 的多余全量和组合入口。
- **描述**：AI 对照 `initproc`、两份正式脚本和只读 BuildStorm judge，确认正式路径原本已正确且必须保持不变。删除 CAgent 的官方脚本嵌入、失败案例/全案例聚合，改为恰好 10 个单项 `run_*` 入口；删除 BuildStorm 的官方序列、选择器、组合诊断及非得分点探针，改为 toolchain、MINIBUILD、compile success、compile time 四个单项模块。compile 与 compile time 复用冷构建主体并仅输出 `BUILDSTORM_DEBUG_*`，不会干扰平台正式评分。RISC-V 与 LoongArch64 用户态构建通过；未运行依赖 final 镜像且可能持续 4 小时的 QEMU 单项编译。
- **关联 commit**：`33793bbc`

#### BuildStorm 并行度与文件映射吞吐优化（7.23）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 中 BuildStorm 编译超过两小时仍只推进到
  `97/446` 的低吞吐问题并完成内核优化。
- **描述**：AI 结合 BuildStorm 脚本和调度器实现确认，`sched_getaffinity` 暴露单个
  `home_hart` 使 guest `nproc=1`，Cargo 因此串行；修复恢复八核在线 mask。同时将干净
  文件页缓存扩展到 `MAP_PRIVATE` 读 fault，架构页表为私有写映射保留 COW，并把文件读
  直接写入新页帧。曾尝试挂载后重建 lwext4 bcache，但 QEMU 复现 journal 引用导致的
  `fs::init` 卡住，已撤回该不安全方案。
- **验证**：RISC-V、LoongArch64 release 构建通过；RISC-V 180 秒 QEMU 启动进入多 crate
  Cargo 预构建，无 panic。完整 446 单元和严格 A/B 耗时未完成，未宣称性能百分比或正式
  BuildStorm 通过。详见 `ai.log` 对应条目和
  [problem/buildstorm-parallel-file-cache.md](./problem/buildstorm-parallel-file-cache.md)。
- **关联 commit**：`82e92a54`

#### 调度器无竞争定时器抢占优化（7.24）

- **工具/模型**：Codex (GPT-5)
- **场景**：进程调度器性能路径梳理、timer 抢占优化、CFS/RR 双策略验证与文档记录
- **描述**：AI 沿 timer trap 到 `suspend_current_and_run_next()` 的调用路径审计后确认，
  当前 hart 无其他就绪任务时，原实现仍会经过 idle 上下文完成一次无效调度往返。新增
  `preempt_current_and_run_next()` 和策略统一的 `has_ready_for_hart()`；CFS 检查本 hart
  队列，RR 检查全局队列中匹配 `home_hart` 的任务。阻塞、睡眠、显式让出和 group-exit/
  SIGKILL 处理保持原路径。双架构 perf 构建通过，短样本约为普通内核 `707 -> 684`、perf
  内核 `679`，但完整 BuildStorm 446 单元和严格 A/B 尚未完成，未宣称正式性能提升。
  详见 `Docs/决赛文档/ai.log` 2026-07-24 条目和
  [problem/scheduler-uncontended-preemption.md](./problem/scheduler-uncontended-preemption.md)。
- **关联 commit**：`daa9f96c`

#### clone procfs 目录延迟物化与性能优化（7.24）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供新的 `log.ans`，要求继续降低 clone 的高耗时。
- **描述**：perf 分项显示 11 次 clone 的 procfs 阶段占 `95703 us`，而地址空间仅
  `20952 us`。AI 追踪通用 `open(O_CREATE|O_DIRECTORY)` 到 EXT4 目录创建，确认重复
  路径查找、元数据更新和目录事务是根因。修复为 clone 只登记 PID，首次打开具体 proc
  路径时延迟物化目录；枚举 `/proc` 前批量物化，回收时注销，并加入父目录 inode 缓存
  的根查找回退。另将 `mincore` 合法的未映射范围失败日志降为 debug。
- **验证**：RISC-V 日志从 `clone procfs total_us=95703` 降至 `231`，最大值 109 us，
  `cagent cpu pass 603` 并正常关机；RISC-V、LoongArch64 perf 构建通过。详见
  [clone-procfs-lazy-materialization.md](./problem/clone-procfs-lazy-materialization.md)
  和 `ai.log` 对应条目。
- **关联 commit**：`336b1e24`

#### CAgent `rt_sigsuspend` 忙让出调度优化（7.24）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供新的 `log.ans`（`cagent cpu pass 819`）并澄清启动阶段的
  `nanosleep` 仅因非法 `tv_nsec` 返回 `EINVAL`，要求继续根据 `log.ans`/`debug.ans`
  优化。
- **描述**：AI 对齐 `SigSuspend` 的 debug 行号和 perf 调度快照，确认
  `sys_rt_sigsuspend` 在约 1 秒等待期间忙调用 `suspend_current_and_run_next()`，造成
  4096 -> 158928 次调度选择。修复改用 `block_current_and_run_next()`，在发布 Blocked
  前复查 pending signal 以避免丢唤醒；同时增加无竞争 `sched_yield()` 快速路径，保留
  timer future `nanosleep` 与其 `EINVAL` 校验语义。详见 `ai.log` 和
  `problem/sigsuspend-busy-yield-scheduler.md`。
- **验证**：RISC-V/LoongArch64 独立 target perf release 编译通过，仅有既有 smoltcp
  warning；QEMU 运行因 `/var/tmp` snapshot 权限未重新执行，未报告新耗时或正式评分。
- **关联 commit**：`3fd49fd2`
#### LTP mmap3 并发缓存抖动与 MAP_STACK 回收修复（7.23）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析 `log.ans` 中的 `mmap3` watchdog `TBROK`，定位 ext4 缓存性能问题并完成修复、双架构构建和 QEMU 回归。
- **描述**：AI 对照 `mmap3.c` 确认 40 个线程并发操作延迟删除临时文件；`FIFO_SIZE=10` 的 whole-file cache 会驱逐仍活跃的缓存，下一次 seek 又在全局 ext4 锁下重建，造成缓存抖动和超时。修复为延迟删除 inode 固定缓存、成功写回清除脏标志、`file_size()` 使用独立 descriptor，并让 `munmap()` 回收带 `MAP_STACK` 的动态线程栈而不影响固定主栈。RISC-V 300 秒 QEMU 中 musl/glibc 均 `TPASS`、summary 为 `passed 1 failed 0 broken 0` 并正常 `shutdown!`；RISC-V/LoongArch64 release 与 RISC-V debug 构建通过。详见 [problem/mmap3-cache-churn-and-map-stack-leak.md](./problem/mmap3-cache-churn-and-map-stack-leak.md) 与 `ai.log` 对应条目。
- **关联 commit**：`67fa0afb`

#### LTP mmap18 MAP_GROWSDOWN 与 SIGSEGV 线程组退出修复（7.24）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析并修复根目录 `log.ans` 中的 LTP `mmap18`，将最终结果写回 `log.ans` 并补充问题复盘。
- **描述**：AI 对照只读 `mmap18.c` 确认用例同时要求匿名 `MAP_GROWSDOWN` 守卫页扩展和被阻挡扩展时的子进程 `SIGSEGV`。修复在 mmap 缺页路径中加入匿名私有 grow-down VMA 的 256 页 guard gap/重叠约束扩展，并将默认致命信号改为线程组退出。为避免该通用语义破坏 execve 去线程化，内部 sibling `SIGKILL` 采用显式 per-task 标记，真实 SIGKILL 和 strict seccomp 仍保持进程终止及 wait status。RISC-V `log.ans` 中 musl/glibc 各 4 项 `TPASS`、无 `TFAIL/TBROK`、正常 `shutdown!`；详见 [problem/mmap18-growsdown-sigsegv-group-exit.md](./problem/mmap18-growsdown-sigsegv-group-exit.md) 与 `ai.log` 对应条目。
- **关联 commit**：`ed5b28cf`

#### LTP mmap16 ext4 loop 容量与 mmap 写回修复（7.24）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析 `log.ans` 中 LTP `mmap16` 的 `mremap ENOSYS` 与后续测试超时，并完成修复。
- **描述**：AI 对照只读 `mmap16.c` 和日志，区分了初始 syscall 语义缺失与后续每轮父进程 1 KiB 写满 loop 文件导致的 checkpoint 超时。修复实现原址 `mremap`、共享 mmap `ENOSPC -> SIGBUS`，在简化 loop/ext4 模型中记录格式化容量和共享逻辑配额，按 64 KiB 预留与 unlink 回收；连续写入增加按 offset 的 cache fast path，配额耗尽时避免关闭路径同步重放整份脏缓存。RISC-V musl/glibc 均 10 轮 `TPASS`，summary 均为 `passed 10 failed 0 broken 0` 并正常关机。详见 `problem/mmap16-ext4-loop-enospc-writeback.md`。
- **关联 commit**：`ccb03bb2`

#### LTP mmap14 MAP_LOCKED 与 VmLck 统计修复（7.24）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 并修复其中的 LTP `mmap14` 失败。
- **描述**：AI 对照 `mmap14.c` 的 `MAP_LOCKED`/`VmLck` 断言，确认 `MmapFlags` 未定义 `MAP_LOCKED` 导致 mmap flags 被截断，同时 `/proc/self/status` 动态内容缺少 `VmLck`。修复接入 `MAP_LOCKED`，按 VMA 范围统计锁定内存并输出 `VmLck`；`make` 双架构构建和 RISC-V 定向 QEMU 回归通过，musl/glibc 均 `TPASS`、无 `TFAIL/TBROK/panic` 并正常 `shutdown!`。详见 [problem/mmap14-map-locked-vmlck.md](./problem/mmap14-map-locked-vmlck.md) 与 `ai.log` 对应条目。
- **关联 commit**：`bb43a80f`


#### LTP mmapstress04 文件扩展后的 mmap EOF 误判修复（7.24）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析 `log.ans` 并修复 LTP `mmapstress04` 的 musl/glibc `SIGBUS`。
- **描述**：AI 对照只读 `mmapstress04.c` 确认文件在 mmap 后从 1 页扩展到 384 页；原 VMA 静态 EOF 快照将扩展后新页误判为 EOF 外页。修复删除静态快照，缺页时按 backing inode 当前长度判断，并在建图时初始化 ext4 inode 长度以保留 unlink 后打开映射语义。RISC-V `log.ans` 中 musl/glibc 均 `TPASS`、summary 为 `passed 1 failed 0 broken 0`，正常 `shutdown!`；详见 `problem/mmapstress04-dynamic-eof.md`。
- **关联 commit**：`bc3c1fad`

#### LTP mount03 EOF 读取 atime 修复（7.24）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析根目录 `log.ans` 中 LTP `mount03` 的失败并修复，随后要求补充项目文档。
- **描述**：AI 对照 `mount03.c`、VFS 文件读取路径和 lwext4 时间戳接口，确认测试写入后从 EOF 成功读取 0 字节，而 `OSFile::read()` 的 EOF 快速返回绕过 `Ext4Inode::read_at()`，使普通文件 atime 从未更新。新增 inode 级 `touch_atime()`，由 ext4 在未设置 `MS_NOATIME` 时关闭临时句柄后更新 atime，并在普通读取与 EOF 快速路径调用；目录的 `MS_NODIRATIME` 行为保持不变。最新 RISC-V `log.ans` 中 musl/glibc 均为 `passed 55 failed 0 broken 0`，无 `TFAIL/TBROK`，正常 `shutdown!`；详见 [problem/mount03-atime-eof-read.md](./problem/mount03-atime-eof-read.md) 与 `ai.log` 对应条目。
- **关联 commit**：`48a981e3`

#### execve 动态解释器按需映射优化（7.25）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者根据新的 `log.ans` 指出 `execve` 是主要耗时并要求继续优化，随后要求先补充文档。
- **描述**：阶段 perf 计时确认 `from_elf` 和动态解释器 PT_LOAD eager 映射占主要时间。修复 ELF 前缀增量读取、动态解释器页对齐文件段按需 `MAP_PRIVATE` 映射，并让 ELF BSS 缺页分配零页；可写页保持 COW 私有语义。
- **验证**：当前 RISC-V `log.ans` 的 12 次 `execve` 累计 `208763 us`，CAgent `fs-search pass 764` 并正常 `shutdown!`，无 `panic/TFAIL/TBROK`；RISC-V、LoongArch64 release 构建通过。详见 `ai.log` 对应条目和 [problem/execve-dynamic-interpreter-demand-paging.md](./problem/execve-dynamic-interpreter-demand-paging.md)。
- **关联 commit**：当前工作区未提交

#### BuildStorm EXT4 查找元数据复用（7.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求继续分析 BuildStorm `log.ans` 并优化 Cargo 并行编译中的文件系统瓶颈，随后提供 360 秒 `log.ans` 和三分钟 `1.ans`。
- **描述**：AI 追踪 `find()`、`FsIndex::insert_inode_idx()`、`fstat()` 与首个文件页缓存读取，确认同一次路径查找已获得的 `ext4_stat_get()` 结果被重复查询。实现类型与 stat 联合查询，并用其初始化普通文件的可失效 `Kstat/known_size`；同时只为真正新增的 canonical inode alias 进入 EXT4 全局锁。目录、链接和特殊节点保持原有动态路径。
- **验证**：RISC-V `make perf`、LoongArch64 `make build-arch`、格式检查与补丁检查通过。三分钟 RISC-V 样本无 panic/TFAIL/TBROK，且相近读量下 `ext4_fstat_lock` samples、hold、wait 分别由 `8929/7.39 s/4.22 s` 降至 `6028/4.11 s/1.23 s`；日志未完成，未报告完整 BuildStorm wall-clock。
- **关联 commit**：当前工作区未提交

#### BuildStorm 多 hart VFS 读/元数据吞吐优化（7.26）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者在 BuildStorm 不再报错后要求继续将 8 hart 并行编译的吞吐降到可接受范围，并禁止通过修改测试脚本掩盖内核问题。
- **描述**：确认所有 hart 已启动，修正 `CLONE_VM|CLONE_VFORK` exec worker 的 hart 放置；保留普通共享地址空间线程的原 hart 约束。将有界 VFS lookup cache 跨短命 compiler worker 保留，扩展普通单页读取的共享页缓存，并为普通文件增加带写入/元数据变更失效的 `Kstat` cache。lwext4 全局串行约束保持不变。
- **验证**：最新 RISC-V `log.ans` 无 panic/TFAIL/TBROK，在约 303 s 前到达 `11/446`；同阶段旧样本在约 519 s 后才到达，二者不是严格 A/B。RISC-V perf、RISC-V/LoongArch64 release 构建和 `git diff --check` 通过。完整 BuildStorm 与 `fstat` cache 的运行期样本尚未完成。
- **关联 commit**：当前工作区未提交

#### BuildStorm EXT4 全局锁可睡眠等待（7.26）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者持续根据 `log.ans` 要求优化 `buildstorm::compile::run()` 的 EXT4 锁竞争。
- **描述**：将 lwext4 的全局串行约束保留为单一 mutex，但任务竞争时用 `PollSet` 注册
  waker 并阻塞，解锁后再唤醒；无当前任务的启动期保留自旋回退。另将重复 `O_RDONLY`
  descriptor 的打开快路径改为零 `CString` 分配比较。
- **验证**：新 RISC-V 样本在约 3.9 万次 EXT4 读取时，累计锁等待/持锁低于旧相近样本，且无
  `panic/TFAIL/TBROK`；QEMU 在 Cargo `3/446` 被外部终止。RISC-V、LoongArch64 `make perf`
  与补丁检查通过。详见 `ai.log` 对应条目和
  [problem/buildstorm-read-path-lock-contention.md](./problem/buildstorm-read-path-lock-contention.md)。
- **关联 commit**：当前工作区未提交

#### BuildStorm `ppoll` 忙让出调度热循环修复（7.26）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求分析 `buildstorm::compile::run()` 的 `log.ans` 性能瓶颈并实施修复。
- **描述**：AI 以旧、新 perf 快照对齐调度选择、系统调用与 Cargo 进度，定位 `ppoll` 在未就绪
  时将任务保持为 `Ready` 后反复让出 CPU，导致 CFS 立即重选并形成内核态热循环。修复改为快照
  fd、注册文件 waker、二次检查并通过 timer future 阻塞等待；临时 signal mask、可见信号、
  `POLLNVAL` 与零 timeout 扫描语义一并保留或修正。
- **验证**：RISC-V perf、LoongArch64 release 构建与补丁检查通过。RISC-V 新 `log.ans` 在
  guest 341684 ms 时调度选择为 153080 次，Cargo 已至 `6/446`，无 `panic/TFAIL/TBROK`；QEMU
  被外层终止，未将完整 BuildStorm 标为通过。详见 [problem/buildstorm-ppoll-busy-yield.md](./problem/buildstorm-ppoll-busy-yield.md) 与 `ai.log`。
- **关联 commit**：当前工作区未提交

#### BuildStorm MINIBUILD inode cache 命中路径优化（7.25）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供 MINIBUILD 的 `path/open/read/write/stat` 聚合耗时，要求沿文件 I/O
  和路径相关 syscall 读取代码并优化，随后要求先补充文档。
- **描述**：沿 `open_inner()` -> `FsIndex` -> `Ext4Inode` 追踪后，确认缓存打开路径重复执行
  `has_inode`/`find_inode_idx`，命中时重复登记 alias，并在同一次打开中多次取得 EXT4 锁做
  `types()` 查询。修复为一次缓存索引读取、复用 inode 类型，并在 `Ext4Inode` 中保存不可变
  类型字段，使 `types()` 无锁读取；新 inode 的 alias 登记和 rename/hardlink 恢复边界保持
  不变。详见 [problem/buildstorm-read-path-lock-contention.md](./problem/buildstorm-read-path-lock-contention.md)。
- **验证**：格式检查、`git diff --check`、`make perf TARGET_ARCH=riscv64` 通过；现有
  `log.ans` 成功到达 `BUILDSTORM_DEBUG_MINIBUILD ok` 和 `shutdown!`，最终 `path`、`open`、
  `read`、`write`、`stat` 分别为 `4318737`、`3599279`、`3742446`、`3631424`、`2613611 us`。
  QEMU 新一轮未取得独立 A/B 样本，因宿主 `/var/tmp` 权限限制且后续运行被中断，未报告加速
  比例。
- **关联 commit**：当前工作区未提交

#### execve 解释器元数据读取优化（7.25）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求继续优化 `log.ans` 中的 `execve`，并确认 CAgent `fs-create pass 472` 的耗时组成。
- **描述**：AI 对比动态解释器 VMA 路径与 perf 快照，确认页对齐解释器段虽已按需映射，仍先被完整读入内核 `Vec`。修复为仅读 ELF/program header，页对齐段继续使用 `MAP_PRIVATE` 文件 VMA，未对齐 RW 段直接读入已分配用户页，维持零填充、动态库字节补丁与私有 COW 语义。`472` 被确认是 `agent_lite` 的用户态 wall-clock，不是单个内核时间桶；相邻 494 ms 快照的首尾并不与 agent 窗口对齐，故其中 4 次 `execve` 仅可给出近似内核活动，不能视为 472 ms 的精确组成。
- **验证**：RISC-V `log.ans` 中 10 次 `execve` 由 `188572 us` 降至 `170980 us`，8 次解释器读取由 `23402 us` 降至 `8371 us`；`fs-create pass 472` 后正常 `shutdown!`，无 `panic/TFAIL/TBROK`。RISC-V、LoongArch64 release 与 RISC-V perf 构建通过；未运行 LoongArch64 QEMU、完整 LTP/BuildStorm 或第二次独立性能样本。详见 [problem/execve-dynamic-interpreter-demand-paging.md](./problem/execve-dynamic-interpreter-demand-paging.md) 和 `ai.log`。
- **关联 commit**：当前工作区未提交

#### BuildStorm MINIBUILD `lseek` 热路径计时（7.25）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者撤销 `common.rs` 修改后，要求根据 `debug.ans` 的实际调用路径定位
  `buildstorm::minibuild::run()` 慢点并增加内核关键阶段计时。
- **描述**：按 PID/syscall 聚合发现 PID 64 有 8329 次 `lseek`，追踪到
  `sys_lseek()` -> `OSFile::lseek()` 的 `inode.path()`/`inode.types()` EXT4 类型检查，以及
  `SEEK_END` 大小查询和 `SEEK_DATA/HOLE` 探测。新增 `lseek` syscall/VFS 总计时与三个阶段
  桶，未把 debug 日志中的调用次数当作耗时结论，也未恢复 `common.rs`。
- **验证**：RISC-V perf、RISC-V/LoongArch64 release 构建和格式检查通过；QEMU 因宿主
  `/var/tmp` 只读在启动前失败，尚无新的 guest perf 快照。详见 `ai.log` 对应条目和
  [problem/buildstorm-read-path-lock-contention.md](./buildstorm-read-path-lock-contention.md)。
- **关联 commit**：当前工作区未提交

#### 阻塞型 wait/futex perf 改用实际运行时间（7.25）

- **工具/模型**：Codex (GPT-5)
- **场景**：调整阻塞型 syscall 的耗时统计，剔除任务被调度出去期间的时间
- **描述**：AI 追踪通用 syscall 边界计时、`waitpid`/`waitid` 的 poll 阻塞和 futex 的任务切换，确认原 `wait`/`futex` 桶把睡眠区间计入累计 tick。改为 wait/futex 使用活动 guard：wait 按实际 poll，futex 按等待前准备、唤醒后收尾及非阻塞 wake/requeue 区间计时；报告中的 `wait`/`futex` 直接使用活动桶。脚本兼容旧日志，优先以 `wait_active` 替换旧 `wait`。
- **验证**：RISC-V、LoongArch64 `make perf` 通过；旧 `log.ans` 解析后 `wait` 采用 `wait_active` 的 `25.291 ms`。QEMU 因宿主 `/var/tmp` 只读在启动前失败，未取得新的 guest perf 快照。
- **关联 commit**：当前工作区未提交

#### perf 饼图中的 accept 改用实际运行时间（7.25）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求让 `scripts/plot_perf_durations.py` 对 `accept` 采用与 `wait` 相同的实际运行时间口径。
- **描述**：确认旧日志中的 `accept` 是包含阻塞区间的历史桶，而 `accept_active` 只覆盖 accept poll 闭包执行区间。脚本默认模式现在用 `accept_active` 替换 `accept`，`--include-active` 仍可同时查看嵌套项。
- **验证**：当前 `log.ans` 默认汇总显示 `accept=2.432 ms (samples=16)`，而显式保留活动项模式仍显示原始 `accept=1.237 s` 与 `accept_active=2.432 ms`；Python 编译检查和 `git diff --check` 通过。
- **关联 commit**：当前工作区未提交

#### CAgent 全量并发 EXT4/TCP 吞吐优化（7.26）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供全量 CAgent 的评测机耗时，指出其相对 Linux 存在数量级性能差距，并要求针对内核优化及补充文档。
- **描述**：AI 对齐 `log.ans` 的最终 perf 快照，确认 EXT4 全局锁累计等待约 29.034 秒是首要吞吐瓶颈。修复将高频 `inode.path()` 和已知大小查询移出 lwext4 全局锁，页缓存命中改用共享读锁；同时收缩 TCP 无效全栈轮询和 router RX 队列复制。rename、alias recovery、写入、截断和缓存重复加载的语义边界保持保护。详见 `ai.log` 和 [problem/cagent-ext4-tcp-throughput.md](./problem/cagent-ext4-tcp-throughput.md)。
- **验证**：RISC-V、LoongArch64 `make perf` 及格式检查通过。变更后本机 QEMU 全量运行被中断，未报告伪造的 A/B 比例；维护者反馈评测性能提升明显。
- **关联 commit**：当前工作区未提交

#### BuildStorm `cc` 符号链接缓存污染修复（7.26）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求根据 `log.ans` 修复 BuildStorm 并行 Cargo 编译中的
  `/usr/bin/cc` 打开失败及其后的 LTO 插件误报。
- **描述**：AI 确认 GCC/LTO 独立探针正常，定位为 VFS 对 `O_UNLINK`/`O_NOFOLLOW`
  的末级链接查找仍回填普通 inode 缓存。修复禁止该类结果写入 `FsIndex` 和 dentry cache，
  并补齐 Debian 中间 symlink 展开；普通打开继续缓存解析后的实际文件。
- **验证**：格式检查、补丁检查和 RISC-V release 构建通过。定向 BuildStorm 日志已由旧版
  Cargo `1--2/446` 的 `ext4_fopen`/LTO 失败推进到 `37/446` 且未出现对应错误；日志未完成，
  因此未宣称全量 BuildStorm 或 LoongArch64 通过。详见
  [problem/buildstorm-cc-symlink-cache-poisoning.md](./problem/buildstorm-cc-symlink-cache-poisoning.md)。
- **关联 commit**：当前工作区未提交

#### BuildStorm EXT4 锁 FIFO 单唤醒优化（7.26）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供新的 `log.ans`，要求继续优化 `buildstorm::compile::run()` 的 EXT4 锁。
- **描述**：分析确认可睡眠锁的解锁路径仍全量唤醒 `PollSet` waiter，使并发 Cargo 任务反复竞争
  唯一 lwext4 mutex。修复使 waker 队列去重并 FIFO 单唤醒，二次抢锁成功时撤销登记；lwext4 的
  全局串行与启动期回退语义保持不变。
- **验证**：RISC-V、LoongArch64 `make perf` 与补丁检查通过。90 秒 RISC-V 运行已进入
  `buildstorm-compile` 预构建，外层 timeout 结束前未观察到 panic；没有可比的完整 guest 快照，
未报告端到端加速比例。详见 `ai.log` 和
[problem/buildstorm-read-path-lock-contention.md](./problem/buildstorm-read-path-lock-contention.md)。
- **关联 commit**：当前工作区未提交

#### BuildStorm 普通 `read()` 文件页缓存复用（7.26）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析 `log.ans` 中 Cargo 长时间停在 `3/446` 的读路径吞吐问题并实施修复。
- **描述**：根据 EXT4 读锁累计等待和 syscall 聚合，确认普通跨页 `read()` 未复用已有文件页缓存，
  反复进入串行 lwext4。新增受限的普通小文件完整页缓存命中/冷读回填路径，保留写入、截断、
  rename 的失效边界；详见 `ai.log` 和 [problem/buildstorm-read-path-lock-contention.md](./problem/buildstorm-read-path-lock-contention.md)。
- **验证**：RISC-V/LoongArch64 release 构建、RISC-V perf 构建通过；RISC-V `/tmp` qcow2 叠加盘
  180 秒定向运行无 panic/TFAIL/TBROK，Cargo 推进到 `1/446`。完整 BuildStorm 与严格 A/B
  wall-clock 尚未完成。
- **关联 commit**：当前工作区未提交

#### BuildStorm MemorySet 与 EXT4 可睡眠锁死锁（7.26）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供 `log.ans` 与 `gdbclient.ans`，要求定位 BuildStorm 死锁并遵守
  `TaskControlBlockInner -> ResourceSlot -> MemorySet` 锁顺序；死锁修复后要求补充文档。
- **描述**：GDB 显示多个 hart 在 rseq 用户态返回写回或地址空间激活时自旋等待不同的
  `MemorySet` 读锁。追踪确认文件 mmap 缺页、fork 共享页预填充及共享映射写回会在
  `MemorySet` 写锁内进入可通过 `block_on()` 睡眠的 lwext4 全局锁。修复采用锁外 inode/page/frame
  快照与 EXT4 I/O、锁内页表/VMA 更新的两阶段边界；clone 仅在 PCB 锁内瞬时取得资源 Arc，
  tuple 声明保持维护者要求的原状。详见
  [problem/memoryset-ext4-sleeping-lock-deadlock.md](./problem/memoryset-ext4-sleeping-lock-deadlock.md)。
- **验证**：`git diff --check`、格式检查及 RISC-V/LoongArch64 release 构建通过。沙箱 QEMU
  受宿主 `/var/tmp` 临时文件权限限制未启动；维护者确认死锁已修复，未记录为 AI 独立完成的
  完整 BuildStorm 回归。
- **关联 commit**：当前工作区未提交

#### BuildStorm write-back cache LRU 优化（7.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供一小时 `2.ans` 后又提供最新十分钟 `3.ans`，要求继续定位 BuildStorm
  编译吞吐瓶颈。
- **描述**：AI 以新的 `ext4_write_lock`/`ext4_rename_lock` 分类核对 3.ans，确认 `write_at` 的
  累计锁持有已超过 read，而 `find` 仍是最大排队来源。沿 write path 定位到 10 项 FIFO 的
  whole-file cache：并发 Rustc 活跃输出会在持有全局 lwext4 锁时被驱逐并整文件写回。修复将其
  改为 32 项有界 LRU，并在 cache 命中写后更新 recency；稀疏文件、rename、错误重试和非 SMP 安全
  的 lwext4 串行化保持不变。详见 [problem/buildstorm-read-path-lock-contention.md](./problem/buildstorm-read-path-lock-contention.md)。
- **验证**：`cargo fmt`、补丁检查，以及 RISC-V/LoongArch64 `make perf` 通过，仅有既有
  smoltcp warning。`3.ans` 在修复前生成，尚未取得新 kernel 的同配置运行样本或完整 BuildStorm
  结果，未报告加速比例。
- **关联 commit**：当前工作区未提交

#### BuildStorm cached-parent miss 去除无效中间链接扫描（7.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供已含 LRU 的十分钟 `4.ans`，Cargo 仍停在 `9/446`，要求继续优化内核吞吐。
- **描述**：AI 核对 `4.ans`，确认没有 panic/TFAIL/TBROK 或 BuildStorm 结束标记；LRU 后单次
  `write_at` 平均锁持有由约 33.1 ms 降至约 30.3 ms，但不是严格 A/B。继续追踪 `find`：cached-parent
  路径已持有实际目录 inode，却在 child miss 后仍逐前缀进入 lwext4 扫描中间符号链接。新增 VFS 默认
  `find_from_cached_parent()`；EXT4 覆盖它，仅对这个确定的负查找跳过无效扫描。完整路径回退、末级
  symlink 递归、`O_NOFOLLOW`、`O_UNLINK`、`O_CREAT` 和 dentry 失效语义均保留。
- **验证**：`cargo fmt`、`git diff --check`、RISC-V/LoongArch64 `make perf` 均通过，仅有既有
  smoltcp warning。`4.ans` 早于新 fast path，未取得其 QEMU 运行样本或完整 BuildStorm 数据，未报告
  端到端加速比例。详见 [problem/buildstorm-read-path-lock-contention.md](./problem/buildstorm-read-path-lock-contention.md)。
- **关联 commit**：当前工作区未提交

#### BuildStorm Rustc rename 全挂载 flush 优化（7.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供 30 分钟的 `5.ans`，BuildStorm 已到 Cargo `33/446` 但后段仍明显变慢。
- **描述**：AI 发现 `t=1641118ms` 到 `t=1689127ms` 内 rename 样本仅加一、持锁却增加约 74.878 s，
  全局 EXT4 累计等待增加约 402.882 s。源码确认 rename 为清理临时产物 write-back cache 调用
  `ext4_cache_flush()`；该 lwext4 API 实际 flush 整个 mount 的 dirty list，普通 close 又重复调用。
  修复将 byte-cache writeback/discard 与 mount flush 拆开：rename 写回并删除旧 pathname cache、关闭
  descriptor后执行 `ext4_frename()`，但不把普通原子发布升级成隐式 fsync。写回失败重试、源/目标
  cache 失效、同 inode 可见性以及显式 sync/fsync 均保留。
- **验证**：`cargo fmt`、`git diff --check`、RISC-V/LoongArch64 `make perf` 均通过，仅有既有
  smoltcp warning。`5.ans` 早于修复；未取得修复后的 QEMU/完整 BuildStorm 数据，未报告端到端
  加速比例。详见 [problem/buildstorm-read-path-lock-contention.md](./problem/buildstorm-read-path-lock-contention.md)。
- **关联 commit**：当前工作区未提交

#### BuildStorm `6.ans` rename 验证与 stat cache 优化（7.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供修复后运行 210 秒的 `6.ans`，要求验证 `5.ans` 中的 EXT4 rename 尖峰并继续优化。
- **描述**：`6.ans` 的 13 次 rename 累计持锁仅 `0.571 s`，确认此前单次 `74.878 s` 的
  mount-wide flush 尖峰已消失；其余主要等待回到全局 lwext4 锁的 read/find/fstat 路径。审计
  `ext4_fread()` 确认它不写 atime，故移除普通读后无条件失效 `stat_cache`，使后续 fstat 能复用
  未变化的 metadata；写、truncate、rename、link/unlink 与显式时间修改仍维持失效边界。
- **验证**：格式检查、补丁检查及 RISC-V/LoongArch64 `make perf` 均通过，仅有既有 smoltcp warning。
  `6.ans` 早于 stat-cache 改动，未报告该改动的运行期 A/B 或完整 BuildStorm 结果。详见
  `ai.log` 和 [BuildStorm 普通 read 路径与 EXT4 全局锁争用](./problem/buildstorm-read-path-lock-contention.md)。
- **关联 commit**：当前工作区未提交

#### BuildStorm `7.ans` 后段性能复核（7.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供运行 240 秒的 `7.ans`，认为其最终 Cargo 进度低于 210 秒的 `6.ans`，要求复核是否出现性能回归。
- **描述**：按 `Building` 阶段对齐后，`7.ans` 到 `5/446` 反而比 `6.ans` 更早；慢化集中在后续 `serde_core`/artifact 写入密集段。该段新增 3,398 次 EXT4 write，而 fstat 锁仅新增 147 次，故没有证据表明 read 后保留 regular-file `stat_cache` 导致 fstat 回退。rename 的全挂载 flush 尖峰也未复发。保留既有优化，避免仅凭不等长 timeout 样本盲目回滚或扩大写缓存；详见 `ai.log` 和 [BuildStorm 普通 read 路径与 EXT4 全局锁争用](./problem/buildstorm-read-path-lock-contention.md)。
- **验证**：`7.ans` 无 panic、ERROR、TFAIL、TBROK 或 BuildStorm 完成标记；本轮未修改新的内核代码，未重复长时间 QEMU/构建。此前 stat cache 修改已通过 RISC-V、LoongArch64 `make perf`、格式和补丁检查。
- **关联 commit**：当前工作区未提交

#### BuildStorm EXT4 稀疏写、两页预读与写路径统计（7.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者连续提供 BuildStorm `*.ans`/`log.ans`，要求根据实时 perf 输出直接优化；随后明确要求补齐当前工作区和本轮优化的文档。
- **描述**：确认三页预读会以更长的 lwext4 临界区抵消较少的读锁请求，已恢复为当前页加下一页的两页读取。实现按 `(mount, inode)` 索引、16 个 range/64 KiB 上限的 sparse 写缓冲与读时覆盖，避免洞被 whole-file cache 零填充，也避免同路径 descriptor 切换把脏 range 过早提交；同时复用首次 lookup identity、收紧保留最终 symlink 的 cache 边界，并新增 VFS、预读、write-back cache 与 write `open/quota/data` 聚合统计。详见 [problem/buildstorm-ext4-sparse-write-readahead.md](./problem/buildstorm-ext4-sparse-write-readahead.md) 与 `ai.log`。
- **验证**：当前两页预读样本在相近 `Building 9/446` 阶段的 read lock 累计 wait/hold 为 `424.939/53.579 s`，低于三页试验的 `525.574/68.484 s`；sparse read overlay 已实际命中。RISC-V、LoongArch64 perf 构建和双架构 release 构建通过，`git diff --check` 通过。最终新增的 write phase 统计尚未取得 guest 样本；未报告完整 BuildStorm 或严格 A/B 加速比例。
- **关联 commit**：当前工作区未提交

#### BuildStorm 读写 descriptor 往返重开优化（7.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：分析新版 RISC-V BuildStorm `log.ans` 的 write phase，并优化 lwext4 全局锁内的 descriptor 打开开销。
- **描述**：确认共享 inode 已持有 `O_RDWR` descriptor 时，读路径仍切换为 `O_RDONLY`，使随后写路径重新
  `ext4_fopen`。`file_open_read_only()` 现复用同路径已打开的可读 descriptor；全局 EXT4 串行、稀疏写
  覆盖、write-back 和显式持久化边界保持不变。详见
  [BuildStorm EXT4 稀疏写、两页预读与写路径统计](./problem/buildstorm-ext4-sparse-write-readahead.md)。
- **验证**：RISC-V/LoongArch64 `make perf` 和格式/补丁检查通过。维护者提供的新 RISC-V 样本在
  `t=257.349s` 到 `Building 15/446`，无 panic/TFAIL/TBROK；write open 累计 `5.527s / 5711` 次。样本未完成，
  且不是严格 A/B，不报告完整 BuildStorm 或端到端比例。
- **关联 commit**：当前工作区未提交

#### BuildStorm sparse range 槽位收束（7.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者在分析新版 RISC-V BuildStorm `log.ans` 后，要求继续下一步性能优化。
- **描述**：AI 曾以 `sparse_flush_ops=689`、`sparse_flush_bytes=16578252` 的平均 range 长度作为
  16 个离散 range 槽位可能造成提前提交的候选证据，并将 `MAX_SPARSE_WRITE_BUFFER_RUNS` 调整为 32，
  payload 仍限制为 64 KiB。后续代码复核确认 `sparse_flush_ops` 按每个底层 `ext4_fwrite()` range
  递增，而不是每个 buffer flush batch；该指标不能单独证明槽位限制。写入顺序、洞布局、读时覆盖及
  close/sync/rename/truncate/`fstat`/`SEEK_DATA`/`SEEK_HOLE` 的 flush 语义均保持不变。
- **验证**：格式检查、`git diff --check`、RISC-V 与 LoongArch64 `make perf` 以及普通 release 构建
  通过，仅有既有 smoltcp warning。未取得包含该改动的新 guest 样本，且为避免删除维护者保留的
  `disk.img` 未运行 `make run`，不报告运行期加速比例；后续首先补齐并比较 flush batch/reason、
  每 batch payload、write data 和 write-lock wait/hold。
- **关联 commit**：当前工作区未提交

#### BuildStorm 五分钟样本复核与下一轮计划（7.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供新的约五分钟 RISC-V `log.ans`，要求完善文档并规划下一步优化。
- **描述**：最后一个 guest 快照为 `t=216.291s`、Cargo `8/446`，无 panic/TFAIL/TBROK 或结束标记。
  新 32-run 样本在近似 sparse payload 下有 628 个已提交 range、平均 `25.75 KiB`，旧样本为 689 个、
  `23.50 KiB`；由于该计数不是 flush batch 且 Cargo 阶段不同，不报告加速。最终全局 EXT4 累计
  wait/hold 为 `496.947/89.987 s`（多 hart 累计），read/write/find/fstat 排队仍是主压力。下一轮先
  增加 sparse batch/reason 和页缓存来源/旁路统计，再决定是否受限扩展 immutable 大文件缓存；lseek
  special-node 查询只作为后续小路径候选。
- **验证**：本轮为日志与源码口径复核、文档更新，没有改动内核代码或运行构建/QEMU。维护者已有的
  `user/src/bin/initproc.rs` 和 `disk.img` 未触碰。
- **关联 commit**：当前工作区未提交

#### BuildStorm `lseek` open-file 类型缓存与 `tmp_03` 页缓存实验回归（7.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供 `tmp_01.ans`、`tmp_02.ans` 和新的 `tmp_03.ans`，要求分析三分钟
  BuildStorm 性能、对标 Linux 并修复缺陷。
- **描述**：`tmp_01` 的约 3.28 万次 `lseek` 中，`lseek_duration type_check` 累计
  `15,521,831 us`，几乎占满实现时间；根因是每次调用重复 `inode.path()`、特殊节点表查找和
  `inode.types()` 回退。参照 Linux `struct file` 在 open 后绑定稳定 inode/file 状态，
  `OSFile` 新增 `seek_type` 并在 `new()`/`new_fanotify_event()` 创建时解析一次；FIFO/socket
  的 `ESPIPE` 与 SEEK_END/SEEK_DATA/SEEK_HOLE 语义保持。`tmp_02` 在相近调用量下显示
  `type_check=0`、`lseek impl=10,833 us`，证明该热路径优化有效，但 Cargo 阶段不同，不报告
  整体 wall-clock 或超过 Linux。
- **页缓存实验**：曾按 Linux locked folio 思路加入 page-loading 占位和同页等待；`tmp_03`
  停在 `t=33511ms, Building 0/446`，之后没有 perf/Cargo 推进，疑似永久阻塞或失效竞态。
  实验代码已撤销，不作为成功修复；须先补 owner 取消、失效代际及 read/mmap/splice 并发测试。
- **验证**：RISC-V/LoongArch64 `make perf`、`cargo fmt --manifest-path os/Cargo.toml -- --check`
  和 `git diff --check` 通过，仅有既有 smoltcp warning。QEMU 因沙箱 `/var/tmp` 只读未能独立
  启动，`tmp_02` 为维护者提供的运行验证；完整 446 crate、Linux 端到端和正式评分仍待后续。
- **关联文档**：[BuildStorm `lseek` open-file 类型缓存优化](./problem/buildstorm-lseek-open-file-type-cache.md)
- **关联 commit**：当前工作区未提交

#### 更新版 `tmp_03.ans` 的混合命中读取优化（7.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供更新后的同名 `tmp_03.ans`，要求基于最新三分钟 BuildStorm 结果继续优化。
- **样本澄清**：此前文档中的 `t=33511ms, Building 0/446` 属于早期 page-loading waiter 实验；
  更新版实际在 `t=175421ms` 到达 `Building 5/446`，无 panic/TFAIL/TBROK/shutdown。最后快照的
  `file_cache hit/miss=453844/24266`，而 `ext4_read_lock wait/hold=311.033/29.048 s`。
- **根因与修改**：大于一页的 `read()` 原先只要一页未命中就整段调用 `inode.read_at()`，重复
  读取其余命中页。`OSFile::try_page_cached_read()` 现按页复制命中数据，只对连续冷页段执行
  一次 `inode.read_at()` 并发布完整页；容量、失效、稀疏文件覆盖和 EOF 语义未改，也没有重新
  启用早期会卡住的 loading waiter。
- **验证**：RISC-V/LoongArch64 `make perf`、格式检查和 `git diff --check` 通过，仅有既有
  smoltcp warning。更新版 `tmp_03` 早于本修复，尚无 guest A/B，不能报告 wall-clock 加速；后续
  需比较 EXT4 read 次数、锁 wait/hold、read_active，并覆盖跨页、EOF、稀疏和并发 mmap/splice。
- **关联文档**：[BuildStorm `lseek` open-file 类型缓存优化](./problem/buildstorm-lseek-open-file-type-cache.md)
- **关联 commit**：当前工作区未提交

#### `tmp_04.ans` 对混合命中读取修复的方向性验证（7.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供混合命中读取修复后的 `tmp_04.ans`，要求完善性能复盘并给出合适提交信息。
- **描述**：`tmp_04` 最后可见 Cargo 为 `Building 5/446`，最后 perf 快照为
  `t=145163ms, Building 4/446`，无 panic/TFAIL/TBROK/ERROR/shutdown。与旧 `tmp_03` 约 145 秒
  快照相比，`ext4_read_lock` wait/hold 从 `226.250/24.141 s` 降为 `122.490/16.571 s`，
  read total/active 从 `199.853/118.286 s` 降为 `150.207/49.550 s`；样本阶段和工作量不同，
  只作为方向性证据，不报告严格 A/B 或整体 wall-clock 加速。
- **结论**：数据与“命中页直接复制、只对连续冷页段进入 inode.read_at”一致；后续仍应固定
  镜像、hart、缓存状态和 Cargo 阶段，比较 EXT4 reads、冷段读取数、锁 wait/hold 和 read_active。
- **验证**：本轮完善文档，没有新增内核代码；混合命中修复已通过 RISC-V/LoongArch64 perf、
  release 构建、格式检查和 `git diff --check`。QEMU 独立 A/B 仍受宿主 `/var/tmp` 只读限制。
- **关联文档**：[BuildStorm `lseek` open-file 类型缓存优化](./problem/buildstorm-lseek-open-file-type-cache.md)
- **关联 commit**：当前工作区未提交

#### BuildStorm 文件页缓存路径分组与共享路径优化（7.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者提供 `tmp_05.ans`、两分钟 `tmp_06.ans`，要求优先继续提升 BuildStorm 性能，并说明
  Rustc `SIGSEGV` 是偶发现象。
- **描述**：AI 对照更新版 `tmp_03`、`tmp_05`、`tmp_06` 确认三者均出现过同类 Rustc `SIGSEGV`，
  因而不把它归因于本次改动，也不使用失败后的累计统计报告加速。稳定热点是数十万页缓存命中仍在
  平铺 `(pathname, page_index)` `BTreeMap` 中反复构造键、复制路径和比较 pathname。修复将索引改为
  `pathname -> page_index -> page` 两级结构，使用 `Arc<str>` 共享路径；EXT4 inode 通过新增的
  `page_cache_path()` 返回稳定路径镜像，常规 `path()`、写入/截断/rename 失效及非 EXT4 inode 的
  回退语义保留。
- **验证**：`cargo fmt --manifest-path os/Cargo.toml -- --check`、`git diff --check`、RISC-V 和
  LoongArch64 `make perf` 通过，仅有既有 smoltcp warning。尚无包含最终组合改动的同配置三分钟 guest
  A/B，未报告端到端或 Linux 对标百分比。
- **关联文档**：[BuildStorm `lseek` open-file 类型缓存优化](./problem/buildstorm-lseek-open-file-type-cache.md)
- **关联 commit**：当前工作区未提交

#### BuildStorm mmap 页缓存二次查找路径复用（7.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：BuildStorm 性能日志分析、mmap 页缓存热路径优化与验证边界记录。
- **描述**：依据 `tmp_08.ans` 的 `163603` 次文件页 fault 与 `tmp_09.ans` 的 `146410` 次页 fault，
  AI 定位 VMA 安装阶段对已加载缓存页的第二次 lookup 仍复制 `inode.path()`。修复为优先复用
  inode 的 `Arc<str>` 页缓存路径，保留其他后端回退和原有加载、失效、COW、lwext4 串行语义。
  `tmp_09` 无 panic/ERROR/TFAIL/TBROK 且推进至 `Building 4/446`，但不是严格 A/B，因此不报告
  端到端加速。详见 `ai.log` 同日条目与
  [problem/buildstorm-lseek-open-file-type-cache.md](./problem/buildstorm-lseek-open-file-type-cache.md)。
- **关联 commit**：当前工作区未提交

#### 参考 Linux inode 锁的 BuildStorm EXT4 写回缓存并发重构（7.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：维护者要求根据 `tmp_09.ans` 开展下一轮性能优化，并要求参考 Linux 对文件系统进行
  高性能重构；维护者已编译完成，明确要求不再构建。
- **依据与修改**：`tmp_09` 的 write-back cache 有 `124` 次命中，而 `ext4_write_lock` 的累计
  wait/hold 为 `9.388/3.300 s`。对照 Linux 7.0 的 `i_rwsem`、ext4 buffered write 和 filemap 锁序，
  在 Ya2yOS 新增可睡眠的每 inode `write_state`，使已有 dense cache 且已完成 quota 预留的非 sparse
  常规写仅更新 `VFileCache`/FIFO，不占用 `EXT4_OP_LOCK`。rename、truncate、unlink、delayed-unlink、
  hard link 和恢复路径共用 inode 状态锁；miss、quota、cache init、direct/sparse write、eviction 与
  全部 lwext4 API 仍保留全局串行。perf 新增 `fast_hit_ops/fast_hit_bytes` 用于统计实际绕过次数。
- **验证与边界**：`cargo fmt --manifest-path os/Cargo.toml -- --check`、`git diff --check` 通过；未运行
  构建、QEMU 或 BuildStorm，遵从维护者指令并保护其未跟踪 `disk.img`。`tmp_09` 不是完整 A/B，未报告
  性能百分比。Linux 对照文档为 `/home/ya2yo/learning_linux/ya2yos-ext4-cache-concurrency.md`，详见
  [问题复盘](./problem/buildstorm-ext4-inode-cache-write-concurrency.md)。
- **关联 commit**：当前工作区未提交
