# 问题复盘索引

开发过程中遇到的问题，按主题拆分为单文件。新增问题时在此目录新建 `.md` 并更新本索引。

## 进程 / 线程 / 信号

- [sys_clone行为](./clone.md)
- [pending导致死循环](./block-on-pending.md)
- [单独运行cgroup_fj_proc 卡死](./cgroup-fj-proc.md)
- [futex 信号中断后残留 Waiter 导致重复唤醒 panic](./futex-waiter-panic.md)
- [wait4 阻塞未处理信号导致死循环](./wait4-signal.md)
- [进程退出托孤 exit_and_reparent](./exit-reparent.md)

## 内存 / 页表

- [龙芯架构 busy_box 测试失败](./loongarch-busybox-cow.md)
- [内核堆碎片化：全量测例运行 OOM](./fsindex-oom.md)

## 文件系统 / 动态链接

- [龙芯架构 iozone-glibc 动态链接与缺页处理修复](./iozone-glibc.md)

## 测例与驱动

- [lmbench](./lmbench.md)
- [龙芯架构 libcbench_testcode](./loongarch-libcbench.md)
- [龙芯架构添加网络模块后卡在汇编阶段](./loongarch-net-init-hang.md)

## 网络

- [accept02 组播与 setsockopt 错误传播](./accept02-mcast-setsockopt.md)
