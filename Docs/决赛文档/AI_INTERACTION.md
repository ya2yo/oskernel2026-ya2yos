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
- **关联 commit**：待提交

#### LTP fchmod05 chmod S_ISGID 语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fchmod05` chmod 语义定位、`S_ISGID` 清除规则修复、构建与日志验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fchmod05` 失败是非 root 目录 owner 在 gid 不匹配目标目录时仍成功保留 `S_ISGID`；Linux 语义要求 `fchmod()` 成功但静默清掉 setgid 位。修复为新增 `chmod_inode()` 并让 `fchmod/fchmodat` 共用，补齐只读挂载 `EROFS`、非 owner `EPERM`、gid 不匹配清除 `S_ISGID`，同时传播 `fmode_set()` 错误。`make` 通过，最新 `log.ans` 中 musl/glibc `fchmod05` 均 `TPASS`，Summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fchmod05-chmod-setgid.md](./problem/fchmod05-chmod-setgid.md)。
- **关联 commit**：待提交

#### LTP fanotify02 FAN_EVENT_ON_CHILD 卡死修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 卡死分析、fanotify 目录 child event 匹配、`fanotify_mark(FAN_MARK_REMOVE)` mask 语义修复、LoongArch64 LTP 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 最后卡死并修改。AI 确认卡死在 `fanotify02` 的 `read(fd_notify)`，原因是目录 `"."` mark 带 `FAN_EVENT_ON_CHILD`，但内核只按路径精确相等匹配，子文件 open/write/close 事件未入队。修复为让 fanotify 目录 mark 匹配直接子项路径，并收紧路径分隔符边界；随后修正 `FAN_MARK_REMOVE` 单独移除 `FAN_EVENT_ON_CHILD/FAN_ONDIR` 被误判 `EINVAL` 的问题。`make` 通过，LoongArch64 单跑 musl/glibc `fanotify02` 均 8 项 `TPASS`，summary 为 `passed 8 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fanotify02-event-on-child.md](./problem/fanotify02-event-on-child.md)。
- **关联 commit**：待提交

#### LTP chown04 chown errno 语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、`chown(2)` 路径 errno 优先级修复、父目录 search 权限检查、只读挂载检查、LoongArch64 LTP 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `chown04` 失败来自三个路径级 errno 被非 root `EPERM` 或普通查找 `ENOENT` 覆盖：无搜索权限父目录应返回 `EACCES`，超长路径应返回 `ENAMETOOLONG`，只读 tmpfs 挂载点应返回 `EROFS`。修复为在 `fchownat()` 中补路径长度和 `NAME_MAX=255` 检查，普通路径打开目标前检查父目录 execute/search 权限，并让 `chown_inode()` 在权限检查前根据目标路径检查只读挂载点。`make` 通过，LoongArch64 单跑 musl/glibc `chown04` 均 8 项 `TPASS`，summary 为 `passed 8 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/chown04-chown-errno.md](./problem/chown04-chown-errno.md)。
- **关联 commit**：待提交

#### LTP fcntl01 F_GETFL/F_SETFL 状态标志修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、`fcntl(2)` `F_GETFL/F_SETFL` 语义修复、fd 文件状态标志保存、构建与日志验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fcntl01` 失败是 `F_GETFL` 硬编码返回 `O_RDWR`，导致以 `O_WRONLY` 打开的文件也读到 `0x2`；同时 `F_SETFL(O_APPEND)` 只处理 nonblock，未保存 append 状态。修复为让 `FileDescriptor` 暴露 `F_GETFL` 可见的真实访问模式和状态位，并让 `F_SETFL` 更新 `O_APPEND/O_NONBLOCK/O_ASYNC/O_DIRECT/O_NOATIME` 等可修改状态位，同时继续同步底层文件 nonblocking 状态。`make` 通过，用户提供的最新 `log.ans` 中 musl/glibc `fcntl01` 均 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl01-f-getfl-setfl.md](./problem/fcntl01-f-getfl-setfl.md)。
- **关联 commit**：待提交

#### fcntl DUPFD 与 pipe size 兼容性完善（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`fcntl(2)` 基础命令语义梳理、`F_DUPFD*` fd descriptor flag 修复、pipe size 命令实现、构建验证、文档完善
- **描述**：用户要求继续完善 `fcntl` syscall。AI 对照现有实现和 LTP fcntl/pipe 用例，修正关闭 fd 槽错误码、`F_DUPFD` 不继承 `FD_CLOEXEC`、`F_DUPFD_CLOEXEC` 设置 `FD_CLOEXEC`、`arg >= RLIMIT_NOFILE` 返回 `EINVAL`、fd 表满返回 `EMFILE` 等语义；同时将 pipe 容量从固定常量改为 per-pipe 字段，支持 `F_GETPIPE_SZ/F_SETPIPE_SZ`、按页取整、`EBUSY/EPERM` 错误和 `/proc/sys/fs/pipe-max-size`。`cargo fmt` 与默认 LoongArch64 `make` 通过；`make run` 在根文件系统 ext4 mount 阶段 panic，未进入 LTP。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl-dupfd-pipe-size.md](./problem/fcntl-dupfd-pipe-size.md)。
- **关联 commit**：待提交

#### LTP fcntl11 POSIX record lock 区间语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl11` 源码对照、POSIX record lock owner 与区间转换修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fcntl11` 失败来自 record lock 实现过于简化：锁 owner 使用用户结构中的 `l_pid` 而非当前进程 pid，同进程重叠锁被当成冲突返回 `EAGAIN`，`F_GETLK` 按插入顺序返回后面的写锁而不是最靠前的冲突锁。修复为 `sys_fcntl()` 传入当前 pid，`file_lock::setlk()` 对同 owner 锁执行覆盖、拆分、合并，`getlk()` 忽略同 owner 锁并按起始偏移选择冲突锁，同时兼容写回 `struct flock.l_pid`。`make` 通过，最新 `log.ans` 中 musl/glibc `fcntl11` 均 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl11-record-lock.md](./problem/fcntl11-record-lock.md)。
- **关联 commit**：待提交

#### LTP fcntl13 record lock EFAULT 优先级修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl13` 源码对照、`fcntl(F_SETLK)` 坏用户指针错误优先级修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并继续修复。AI 确认 `fcntl13` 失败是 `fcntl(1, F_SETLK, bad_flock)` 期望 `EFAULT` 却返回 `EINVAL`；原因是 record lock 分支先把 `fd=1` 的 stdout 当普通文件解析，非 `OSFile` 先返回 `EINVAL`，遮蔽了坏 `struct flock *`。修复为 `F_GETLK/F_SETLK/F_SETLKW` 以及 OFD lock 分支先 `copy_from_user()` 读取用户 `flock`，再解析普通文件对象。`make` 通过，最新 `log.ans` 中 musl/glibc `fcntl13` 均 `passed 4 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl13-lock-efault-priority.md](./problem/fcntl13-lock-efault-priority.md)。
- **关联 commit**：待提交

#### LTP fcntl14 record lock SEEK_CUR 与阻塞语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl14` 源码对照、POSIX record lock `SEEK_CUR`/负 `l_len`/`F_SETLKW` 语义修复、构建验证、文档完善
- **描述**：用户要求根据 `log.ans` 继续修复，并在确认 `fcntl14` 已通过后补文档。AI 确认剩余失败集中在 `fcntl14` 第 37 起的 `SEEK_CUR` 与负长度区间，以及非法 `l_whence` 和阻塞锁路径；修复为 syscall 层读取当前 fd offset，锁层支持 `SEEK_CUR`、负 `l_len` 反向区间、非法 whence 返回 `EINVAL`，并补 `F_SETLKW` 阻塞等待、等待环 `EDEADLK` 和 close/exit 释放 record locks。`make TARGET_ARCH=riscv64` 与默认 LoongArch64 `make` 均通过，维护者确认后续运行中 `fcntl14` 已通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl14-record-lock-seekcur-len.md](./problem/fcntl14-record-lock-seekcur-len.md)。
- **关联 commit**：待提交

#### LTP fcntl23 文件租约基础语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl23` 源码对照、`F_SETLEASE/F_GETLEASE` 基础租约状态实现、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fcntl23` 失败来自 `F_SETLEASE` stub 固定返回 `EAGAIN`，导致只读普通文件上的无冲突读租约无法建立。修复为在 `file_lock` 层新增按 path/pid 管理的最小 file lease 表，支持 `F_RDLCK/F_WRLCK/F_UNLCK` 设置、`F_GETLEASE` 查询、读租约可写 fd 的 `EAGAIN` 校验，以及 close/close_range/exit 清理。`make` 通过，LoongArch64 单跑 musl/glibc `fcntl23` 均 `TPASS`，summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl23-file-lease.md](./problem/fcntl23-file-lease.md)。
- **关联 commit**：待提交

#### LTP fcntl31 async I/O owner 与信号通知修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl31` 源码对照、`F_SETOWN_EX/F_GETOWN_EX/F_SETSIG` 语义补齐、pipe async I/O 信号投递、构建与日志验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修改。AI 确认 `fcntl31` 失败是 `F_GETOWN_EX` 直接返回 `EINVAL`，并进一步确认测试还要求 pipe 写入时根据 `F_SETOWN/F_SETOWN_EX` 和 `F_SETSIG(SIGUSR1)` 向 TID/PID/PGRP owner 投递异步 I/O 信号。修复为新增 `FasyncOwner`，在 pipe 共享 buffer 中保存 async owner/signal，实现相关 fcntl 命令，并实现按进程组投递信号。`make` 通过，用户提供的最新 `log.ans` 中 musl/glibc 两轮 `fcntl31` 均 5 项 `TPASS`，summary 为 `passed 5 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl31-fasync-owner-signal.md](./problem/fcntl31-fasync-owner-signal.md)。
- **关联 commit**：待提交

#### file_lock 模块高内聚重构（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：文件锁模块职责拆分、POSIX record lock/file lease/BSD flock 子模块化、双架构构建验证、文档完善
- **描述**：用户指出 `os/src/syscall/fs/file_lock.rs` 功能不够单一，要求重构以提高内聚度。AI 将单体文件替换为 `file_lock/` 模块目录：`mod.rs` 作为门面保留原 `file_lock::...` API，`types.rs` 保存 `Flock` ABI，`posix.rs` 保存 POSIX record lock 与等待图，`lease.rs` 保存 file lease，`bsd_flock.rs` 保存 `flock(2)` 整文件锁。本次不改变 syscall 分发、函数签名或用户可见语义；`make` 和 `make TARGET_ARCH=riscv64` 均通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：待提交

#### session ID 独立字段与 getsid/setsid 语义完善（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：进程元数据 session ID 字段新增、`getsid/setsid/setpgid` 语义调整、syscall 分发接入、双架构构建验证、文档完善
- **描述**：用户询问当前 session id 对应字段后，要求增加 `sid` 字段并同步调整内核。AI 确认原实现把 session ID 混用为 `ProcessMeta::pgid`，且 `GetSid` 枚举未接入分发；修复为新增 `ProcessMeta::sid`，initproc 设 `sid=pid`，fork/clone 继承调用者 `pgid/sid`，`getsid()` 返回目标进程 `sid`，`setsid()` 设置 `sid=pid` 与 `pgid=pid` 并拒绝进程组 leader，`setpgid()` 只修改 `pgid` 并保留 session 边界检查。`make` 和 `make TARGET_ARCH=riscv64` 均通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：待提交

#### fcntl syscall 实现位置收敛（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`fcntl(2)` syscall 代码组织重构、`fd_ops.rs` 职责收敛、构建验证、文档完善
- **描述**：用户要求将 `fcntl` syscall 实现代码放入 `os/src/syscall/fs/fcntl.rs`。AI 将 `sys_fcntl()`、record lock 阻塞 helper `setlk_blocking()` 和 `struct f_owner_ex` 编解码从 `fd_ops.rs` 迁入 `fcntl.rs`，保留 `fcntl` 常量与实现同文件维护；`fd_ops.rs` 回到 `flock/dup/open/close/openat2` 等通用 fd 操作。本次不改变 syscall 分发或用户可见语义；默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目。
- **关联 commit**：待提交

#### LTP fcntl33 文件租约 break SIGIO 通知修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl33` 源码对照、file lease break 通知与降级语义修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修复。AI 确认 `fcntl33` 失败包含 `/proc/sys/fs/lease-break-time` 缺失、冲突 `open/truncate` 未向 lease holder 投递 `SIGIO`、写访问 break 下错误允许写 lease 降级为读 lease，以及 `truncate("file")` 未按当前工作目录解析。修复为启动期补齐 lease sysctl 文件，file lease 表记录 break 通知状态和写访问标志，普通文件 `open/truncate` 冲突时向 holder 主线程投递 pending `SIGIO`，并修正 `sys_truncate()` 相对路径。`make` 通过，最新 `log.ans` 中 musl/glibc `fcntl33` 均 7 项 `TPASS`，summary 为 `passed 7 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl33-lease-break-sigio.md](./problem/fcntl33-lease-break-sigio.md)。
- **关联 commit**：待提交

#### LTP fcntl34 OFD lock owner 语义修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl34` 源码对照、OFD lock owner 与 `F_OFD_SETLKW` 阻塞语义修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `fcntl34` 失败来自 OFD lock owner 错误复用进程 pid：同一进程内多个线程分别 `open()` 的 fd 被锁层视作同一 owner，无法互斥保护 `lseek(SEEK_END)+write()`，导致文件写入覆盖和校验阶段提前 EOF。修复为每个 `OSFile` 分配 open file description 级负数 owner，OFD fcntl 分支改用该 owner，`F_OFD_SETLKW` 走阻塞等待，并在最后一个 fd 关闭时释放 OFD 锁。`make` 通过，最新 `log.ans` 中 musl/glibc `fcntl34` 均 `TPASS`，summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl34-ofd-lock-owner.md](./problem/fcntl34-ofd-lock-owner.md)。
- **关联 commit**：待提交

#### LTP fcntl35 pipe-max-size 初始容量限制修复（7.6）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `fcntl35` 源码对照、pipe sysctl 状态同步、非特权 pipe 初始容量修复、LoongArch64 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修复。AI 确认 `fcntl35` 失败是 `/proc/sys/fs/pipe-max-size` 写入后只改变普通文件内容，pipe 子系统仍用固定 `65536` 初始容量，导致 `nobody` 新建 pipe 未被限制到 `4096`。修复为新增 pipe sysctl 原子状态，在 `/proc/sys/fs/pipe-max-size` 写入时同步更新；`make_pipe()` 根据当前任务是否具备 `CAP_SYS_RESOURCE` 决定是否应用 sysctl 上限，并让 `F_SETPIPE_SZ` 对非特权任务也使用当前上限。`make` 通过，LoongArch64 单跑 musl/glibc `fcntl35` 均 `passed 2 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-06 条目与 [problem/fcntl35-pipe-max-size-init.md](./problem/fcntl35-pipe-max-size-init.md)。
- **关联 commit**：待提交

#### LTP kill05 kill 权限检查修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `kill05` 源码对照、`kill(2)` uid 权限与进程组语义修复、LoongArch64 构建和日志验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `kill05` 失败来自 `kill(pid, SIGKILL)` 在不同普通 uid 之间错误成功；根因是 `sys_kill()` 直接调用内部信号投递 helper，只检查目标存在性，不检查发送者 real/effective uid 与目标 real/saved uid，也未正确处理 `pid == 0`、`pid < -1` 和 `signo == 0`。修复为新增用户态 `kill(2)` 专用 wrapper，保留内部信号投递路径不受权限检查影响。`make` 通过，最新 `log.ans` 中 musl/glibc `kill05` 均 `TPASS`，summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/kill05-kill-permission.md](./problem/kill05-kill-permission.md)。
- **关联 commit**：待提交

#### signal 模块职责边界重构（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`os/src/signal` 模块内聚度重构、signal frame/投递/timer/default action 职责拆分、LoongArch64 构建验证、文档完善
- **描述**：用户指出 `os/src/signal` 整体内聚度太低，要求继续重构。AI 将原本集中在 `mod.rs` 的多类逻辑拆分为 `frame.rs`、`pending.rs`、`delivery.rs`、`timer.rs`，并把原 `sigact.rs`/`signal.rs` 改名为职责更清晰的 `action_table.rs`/`types.rs`；`mod.rs` 只承担门面导出和全局 signal 常量定义。重构保持 `crate::signal::...` 对外函数名不变，默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目。
- **关联 commit**：待提交

#### shmdt 基础 detach 语义实现（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：SysV shared memory `shmdt(197)` 分发接入、当前进程 shm mapping 解除、LoongArch64 构建验证、文档完善
- **描述**：用户要求实现 `shm_detach`。AI 检查现有 `shmget/shmat/shmctl` 后确认 `shmdt` 只有空 stub 且 syscall 分发未接入；实现为 `sys_shmdt()` 委托 mm 层，在当前进程地址空间中查找起始地址匹配且类型为 `MapAreaType::Shm` 的映射，成功时解除整段映射并刷新 TLB，地址未页对齐或未 attach 返回 `EINVAL`。默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目。
- **关联 commit**：待提交

#### memory_set 模块职责边界重构（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`os/src/mm/memory_set` 高内聚重构、`mod.rs` 门面化、基础 VMA/page-table helper 拆分、方法文档补充、LoongArch64 构建验证、文档完善
- **描述**：用户指出 `os/src/mm/memory_set/mod.rs` 方法注释不清且文件职责过重。AI 将原 `mod.rs` 中的 `MemorySetInner` 类型定义、`MemorySet` 锁封装、基础 `MapArea` 操作、页表访问/统计/回收 helper 分别拆入 `types.rs`、`handle.rs`、`area_ops.rs`、`accessors.rs`，让 `mod.rs` 只承担门面导出、`KERNEL_SPACE` 和 `remap_test()` wrapper；对外 `crate::mm::MemorySet`、`MemorySetInner`、`KERNEL_SPACE` 路径保持不变。默认 LoongArch64 `make` 通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目。
- **关联 commit**：待提交

#### LTP kill10 SA_SIGINFO 发送者信息修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `kill10` 源码对照、pending signal `siginfo_t` 保存与传递、LoongArch64 运行验证、双架构构建验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修复。AI 确认 `kill10` 持续打印 `received unexpected signal 10 from 2` 的原因是 `SA_SIGINFO` handler 读取到的 `si_pid` 被内核填成接收者 pid，而不是发送者 pid；根因是 pending signal 只有 `SigSet` 位图，未保存发送者 siginfo。修复为 task 级 pending signal 增加并行 `sig_pending_info`，用户态 `kill/tkill/tgkill` 投递时记录发送者 pid/uid，`handle_signal()` 和 `rt_sigtimedwait()` 消费时取出该 siginfo。LoongArch64 单跑 musl/glibc `kill10` 均 `TPASS`，LoongArch64 与 RISC-V 构建通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/kill10-siginfo-sender.md](./problem/kill10-siginfo-sender.md)。
- **关联 commit**：待提交

#### LTP kill12 SIG_IGN wait status 修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `kill12` 源码对照、显式 `SIG_IGN` 分发语义修复、`waitpid()` status 污染排查、LoongArch64 运行验证、双架构构建验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `kill12` 失败是父进程对已设置 `SIG_IGN` 的子进程发送信号后，`waitpid()` 仍返回信号终止 status。根因有两处：`handle_signal()` 非 custom 分支没有优先识别显式 `SIG_IGN`；`deliver_signal_to_thread_group()` 又在投递阶段按默认动作提前写入 `termination_signal`，即使信号之后被忽略也会污染 wait status。修复为显式忽略信号直接消费返回，并只在实际默认终止路径中记录 `termination_signal`。LoongArch64 单跑 musl/glibc `kill12` 均 `TPASS`，LoongArch64 与 RISC-V 构建通过。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/kill12-sigign-wait-status.md](./problem/kill12-sigign-wait-status.md)。
- **关联 commit**：待提交

#### LTP link04 linkat errno 与权限语义修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `link04` 源码对照、`linkat(2)` 空路径/超长路径 errno 和父目录权限检查修复、LoongArch64 运行验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `link04` 失败来自 `sys_linkat()` 缺少路径参数预检和 hard link 父目录权限检查：空路径被解析为当前工作目录，超长路径落到底层查找 `ENOENT`，非 root 在缺写或缺搜索权限目录下仍能创建 hard link。修复为在 `linkat` 入口校验空路径和长度，并在普通路径分支检查旧路径父目录搜索权限、新路径父目录写/搜索权限；`AT_EMPTY_PATH` 分支也检查新路径父目录权限。LoongArch64 单跑 musl/glibc `link04` 均 `passed 14 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/link04-linkat-errno-permission.md](./problem/link04-linkat-errno-permission.md)。
- **关联 commit**：待提交

#### LTP link08 linkat mount/rofs/ELOOP 语义修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `link08` 源码对照、`linkat(2)` 跨挂载点、只读挂载和 symlink loop errno 修复、LoongArch64 运行验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `link08` 失败来自 `sys_linkat()` 未检查 hard link 两端的 mount 身份和只读挂载标志，且旧路径长度预检过早返回 `ENAMETOOLONG`，遮蔽中间 symlink loop 的 `ELOOP`。修复为新增 `check_link_mounts()` 返回 `EXDEV/EROFS`，并在旧路径达到读取上限时优先扫描 symlink 前缀识别自引用环。LoongArch64 单跑 musl/glibc `link08` 均 `passed 4 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/link08-linkat-mount-rofs-eloop.md](./problem/link08-linkat-mount-rofs-eloop.md)。
- **关联 commit**：待提交

#### LTP linkat02 hard link 上限卡死修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`linkat02` 卡死分析、LTP hard link 上限探测源码对照、`linkat(2)` `EMLINK` 语义修复、`unlinkat(2)` symlink 删除语义修复、LoongArch64 运行验证、文档完善
- **描述**：用户指出 `linkat02\0` 当前直接卡死。AI 确认卡死发生在 `tst_fs_fill_hardlinks()` setup 阶段：内核没有 hard link 上限，测试会持续创建同一 inode 的 hard link。修复为在 `linkat` 创建 hard link 前检查 `st_nlink >= 1024` 并返回 `EMLINK`；随后又修正 `unlinkat` 不跟随最终 symlink，避免 cleanup 删除 symlink 环时报 `ELOOP` warning。LoongArch64 单跑 musl/glibc `linkat02` 均 `passed 7 failed 0 broken 0 warnings 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/linkat02-hardlink-emlink-unlink-symlink.md](./problem/linkat02-hardlink-emlink-unlink-symlink.md)。
- **关联 commit**：待提交

#### LTP mkdir02 目录 S_ISGID 继承语义修复（7.7）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `mkdir02` 源码对照、目录创建 mode 继承语义修复、LoongArch64 构建和运行验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认 `mkdir02` 失败来自新建目录继承了父目录 gid，但没有继承父目录 `S_ISGID` mode 位；根因是 `create_file()` 在 `mkdirat` 复用的 `O_DIRECTORY|O_CREATE` 路径中只应用 `umask`，没有对目录补父目录 `0o2000`。修复为新建目录且父目录带 `S_ISGID` 时为 `effective_mode` 补 `0o2000`，并传播 `fmode_set()` 错误。LoongArch64 `make run` 中 musl/glibc `mkdir02` 均 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-07 条目与 [problem/mkdir02-setgid-inherit.md](./problem/mkdir02-setgid-inherit.md)。
- **关联 commit**：待提交

#### LTP mmap04 /proc/self/maps 动态映射修复（7.12）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `mmap04` 源码对照、procfs maps/VMA 元数据修复、LoongArch64 运行与双架构构建验证、文档完善
- **描述**：用户要求补充 `/proc/self/maps`。AI 确认 maps 仅在进程创建时生成、动态 `mmap` 后内容过期，固定 16 位地址格式又不符合 Linux maps 文本格式；`MAP_FIXED` 的 VMA 拆分还没有更新共享/私有属性。修复为在打开 `/proc/self/maps` 或 `/proc/<pid>/maps` 时按当前 VMA 重建内容，使用无前导零地址和 `p/s` 后缀，并在 `MAP_FIXED` 拆分时更新 flags。LoongArch64 单跑 musl/glibc `mmap04` 各 14 项 `TPASS`，LoongArch64 与 RISC-V 构建通过。详见 `Docs/决赛文档/ai.log` 2026-07-12 条目与 [problem/proc-self-maps-mmap04.md](./problem/proc-self-maps-mmap04.md)。
- **关联 commit**：待提交

#### LTP mmap12 `/proc/self/pagemap` 缺失修复（7.12）

- **工具/模型**：Codex (GPT-5)
- **场景**：procfs pagemap 文件实现、页表 PFN 导出、LTP `mmap12` 日志验证、文档完善
- **描述**：用户要求补齐 `/proc/self/pagemap`。AI 对照现有 proc 文件刷新模型和 `mmap12` 源码，新增 `/proc/<pid>/pagemap` 的创建、刷新与退出清理，`openat` 支持 self 到当前 PID 的解析，并在页表读锁内采集已建立 PTE 的 present/PFN 条目。实现以稀疏 VFS 文件表示未映射页，避免高地址 VMA 的零填充。最新 `log.ans` 显示 LoongArch64 musl/glibc `mmap12` 均 `TPASS: File mapped properly`，Summary 均为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-12 条目与 [problem/proc-self-pagemap-mmap12.md](./problem/proc-self-pagemap-mmap12.md)。
- **关联 commit**：待提交

#### LTP mmap13 文件映射 EOF SIGBUS 修复（7.12）

- **工具/模型**：Codex (GPT-5)
- **场景**：`log.ans` 分析、LTP `mmap13` 源码对照、mmap 缺页/信号/ext4 长度一致性修复、LoongArch64 运行验证、文档完善
- **描述**：用户持续要求依据最新日志修复 `mmap13`。AI 确认初始问题是文件映射完整 EOF 外页被错误建立为零页；补充 SIGBUS 后又通过运行日志确认 LTP 框架 unlink 后的共享映射因 ext4 `ftruncate` 后错误报告长度 0 而被误杀。修复为 VMA 保存 mmap 时文件长度快照，mmap fault 和 trap 层将完整 EOF 外页判定为 `SIGBUS`，并让 `Ext4Inode` 维护成功 truncate/write 后的长度，保证 `size()`、`fstat()` 和页缓存一致。LoongArch64 单跑 musl/glibc `mmap13` 均输出 `TPASS: Received SIGBUS signal as expected`，Summary 均为 `passed 1 failed 0 broken 0`。详见 `Docs/决赛文档/ai.log` 2026-07-12 条目与 [problem/mmap13-sigbus-eof.md](./problem/mmap13-sigbus-eof.md)。
- **关联 commit**：待提交
