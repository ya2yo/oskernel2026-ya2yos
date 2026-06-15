# 问题复盘索引

开发过程中遇到的问题，按主题拆分为单文件。新增问题时在此目录新建 `.md` 并更新本索引。

## 进程 / 线程 / 信号

- [setresgid(149) 系统调用](./setresgid-syscall.md)
- [sys_clone行为](./clone.md)
- [clone03: fork 后 MAP_SHARED 帧未共享与 recycle panic](./clone-mmap-shared-fork.md)
- [clone04: 缺页未发 SIGSEGV 与 _Fork 语义](./clone04-fork-sigsegv.md)
- [clone05: CLONE_VFORK 挂起机制](./clone05-vfork.md)
- [pending导致死循环](./block-on-pending.md)
- [单独运行cgroup_fj_proc 卡死](./cgroup-fj-proc.md)
- [futex 信号中断后残留 Waiter 导致重复唤醒 panic](./futex-waiter-panic.md)
- [wait4 阻塞未处理信号导致死循环](./wait4-signal.md)
- [waitid 系统调用实现](./waitid-syscall.md)
- [waitid07: WSTOPPED 与 SIGCONT 后 checkpoint 超时](./waitid07-stopped-sigcont.md)
- [waitid08: WCONTINUED 事件缺失](./waitid08-wcontinued.md)
- [waitid10: core dump 信号终止状态](./waitid10-core-dumped.md)
- [waitid11: SIGKILL 终止状态](./waitid11-sigkill-killed.md)
- [waitpid10: zombie PID 复用与进程组等待](./waitpid10-pid-reuse-pgid.md)
- [waitpid13: WUNTRACED stopped child](./waitpid13-wuntraced-stopped.md)
- [wait403: wait4(INT_MIN) errno](./wait403-int-min-esrch.md)
- [进程退出托孤 exit_and_reparent](./exit-reparent.md)
- [线程信号栈检查错误与 futex 退出死锁](./thread-signal-stack-futex-deadlock.md)
- [pthread_robust_detach：地址转换重构 + exit_signal 覆盖 + interrupted 残留](./pthread-robust-detach.md)
- [getrusage03: ru_maxrss / RUSAGE_CHILDREN / proc status](./getrusage03-rusage-proc-status.md)

## 内存 / 页表

- [龙芯架构 busy_box 测试失败](./loongarch-busybox-cow.md)
- [内核堆碎片化：全量测例运行 OOM](./fsindex-oom.md)
- [LoongArch getrusage03 与 QEMU virt 分段内存](./loongarch-getrusage03-split-ram.md)

## 文件系统 / 动态链接

- [LTP access02 execve shebang 脚本](./access02-ltp-execve.md)
- [LTP access01 权限判断与 cleanup 卡死](./access01-permission-cleanup.md)
- [LTP access04 mount / loop 设备修复](./access04-ltp-musl.md)
- [龙芯架构 iozone-glibc 动态链接与缺页处理修复](./iozone-glibc.md)
- [LTP creat04 open 权限检查](./creat04-open-permission.md)
- [sys_linkat 硬链接实现与 lwext4 重构](./linkat-hardlink-refactor.md)
- [fsconfig / fsopen / fsmount 基础实现](./fsconfig-syscall.md)
- [wait402: /proc/sys/kernel/pid_max 缺失](./wait402-pid-max-proc.md)

## 测例与驱动

- [lmbench](./lmbench.md)
- [LTP 包装层 Summary 汇总](./ltp-summary-wrapper.md)
- [龙芯架构 libcbench_testcode](./loongarch-libcbench.md)
- [龙芯架构添加网络模块后卡在汇编阶段](./loongarch-net-init-hang.md)

## 网络

- [accept02 组播与 setsockopt 错误传播](./accept02-mcast-setsockopt.md)
