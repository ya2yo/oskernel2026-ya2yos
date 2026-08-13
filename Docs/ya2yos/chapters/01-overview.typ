#import "../diagrams.typ": flow, relation

= 概述

== 定位与范围
Ya2yOS 是以 Rust 编写的宏内核实验系统，基于 TatlinOS 持续演进，面向 RISC-V64
和 LoongArch64 QEMU 平台提供 Linux 用户态 ABI 的高频兼容路径。内核运行在各架构
的内核特权级；进程、内存、VFS、信号、网络和驱动位于同一内核映像中，由 Rust 类型、
引用计数和显式同步原语约束对象生命周期。

本文档主要从整体上介绍当前内核实现的功能，包括一下几点：系统调用已经覆盖进程、内存、文件、信号、时间、
同步、网络和 I/O 多路复用等类别；各接口是否具有完整 Linux 语义须以对应章节的
“当前边界”为准。尤其是挂载、虚拟文件系统、异步 I/O、设备模型和部分低频 syscall
仍以兼容实现为主。

== 设计目标

1. *Linux ABI 优先*：通过 Linux syscall 编号、架构 ABI、errno 和用户内存访问规则，确保测例的正常运行；
2. *完整核心链路*：覆盖 ELF 装载、`clone`/`execve`/`wait`、页表和 COW、ext4 文件访问、信号递送、socket 与 VirtIO 设备，使用户负载可从启动到退出闭环运行；
3. *双架构复用*：把页表、陷入上下文、时钟和设备传输差异隔离在 `arch/` 与`drivers/virtio/`，让任务、内存、文件和 syscall 高层逻辑共用；


== 模块组织

`os/src/main.rs` 负责按依赖顺序初始化子系统；`arch`、`drivers` 提供平台能力，
`trap` 和 `syscall` 构成用户态 ABI 入口，其他模块保存核心状态和语义。当前目录边界如下：

```text
os/src/
├── arch/       # RISC-V64 / LoongArch64 上下文、页表、时钟、trap 与平台代码
├── drivers/    # VirtIO block/net、设备容器及架构相关 transport
├── trap/       # 用户陷入、页故障、时钟中断与返回用户态
├── syscall/    # Linux syscall 分发；task/mm/fs/net/signal/sync/io_mpx 等 ABI 入口
├── task/       # Process、TCB、可选 CFS/RR 调度、futex、clone/exit/wait 支撑
├── mm/         # MemorySet、VMA、帧分配、ELF、用户复制、COW、共享内存
├── fs/         # ext4/VFS 适配、fd、pipe、epoll、设备、挂载记录与 proc 兼容
├── signal/     # action 表、pending、递送、信号帧和 interval timer 信号
├── net/        # smoltcp service、路由、TCP/UDP/Unix socket 与网络设备包装
├── timer/      # 时钟、超时、rusage 与时间 ABI 数据类型
├── sync/       # 内核同步封装
└── utils/      # errno、ID、poll、资源槽位和通用辅助类型
```

#figure(
  flow((
    [*用户态*\应用、libc、测试程序],
    [*ABI 入口*\trap、syscall、用户复制],
    [*内核服务*\task、mm、fs、signal、net、timer],
    [*平台能力*\arch、VirtIO drivers、QEMU]
  )),
  caption: [Ya2yOS 的分层关系：上层经 ABI 使用内核服务，平台差异收敛到架构与驱动层。],
)

== 启动与用户态主线

汇编入口位于 `arch/*/qemu/asms/entry.asm`。首个 hart 进入 `rust_main()` 后依次完成
时钟频率、内存、日志、trap、任务、文件系统和网络初始化；随后创建 `/initproc` 对应的
初始 TCB，发布启动屏障并使能定时器。`run_tasks()` 将 runnable 任务交给
`task/scheduler/` 中由 feature 选定的策略，再取出下一任务；默认 CFS 按最小
`vruntime` 选择，RR 配置按 FIFO 轮转。首次运行由 `trap_return` 恢复用户 trap context。

其他 hart 在 `INIT_FINISHED` 前自旋等待，之后安装 trap 向量、激活内核地址空间并设置
定时器。RISC-V64 与 LoongArch64 的 QEMU 配置分别提供 8 个和 12 个 hart；当前代码已
接入 per-Hart processor、远程入队、空闲 hart 通知、IPI 协作和 remote TLB mailbox，但
尚未形成完整的跨 hart 迁移、work stealing 或负载均衡策略。网络由 `net` feature 控制，
默认启用：RISC-V 在未发现 VirtIO-net 时仍保留 loopback，LoongArch 通过 PCI 路径建立
设备 transport。

#figure(
  flow((
    [`entry.asm`],
    [`rust_main`：mm / logger / trap / task / fs / net],
    [`add_initproc` 与启动屏障],
    [开启 timer interrupt],
    [`run_tasks` → `trap_return` → 第一个用户进程]
  )),
  caption: [从平台入口到第一个用户进程的当前启动主线。],
)

== 用户态接口与对象模型

系统调用号在 `syscall::Syscall` 中定义，以 `num_enum` 将数字转换为枚举并在
`syscall()` 中分发。trap 层从用户寄存器读取 syscall 号和六个参数，处理函数通过
`copy_from_user`、`copy_to_user` 及其 typed wrapper 访问用户内存；`SysErrNo` 最终由
trap 层转换为用户可见的负 errno 返回值。

系统中最重要的对象关系为：`Process` 保存线程组资源（地址空间、fd 表、文件系统
上下文和信号动作表），`TaskControlBlock` 保存一个可调度线程的 trap context、内核栈、
线程级信号与等待状态；`MemorySet` 管理该进程的页表和 VMA；`FileDescriptor` 统一持有
普通文件、管道、socket、事件对象和挂载上下文 fd。

#figure(
  relation((
    [*Process*\MemorySet、FdTable、FSInfo、SigTable],
    [*TCB*\线程上下文、内核栈、pending signal、futex],
    [*FileDescriptor*\file、pipe、socket、event、mount fd],
    [*MemorySet*\页表、VMA、frame 引用]
  )),
  caption: [进程资源、线程执行状态、文件描述符和地址空间的主要归属关系。],
)

== 已实现与限制

内核已具备 ext4 后端、ELF 动态解释器映射、COW、文件 mmap、System V 共享内存、
POSIX 风格信号、TCP/UDP/Unix socket、pipe/eventfd/epoll、VirtIO block/net 与
RISC-V MMIO、LoongArch PCI 传输等主线能力。挂载记录同时支持叠加、bind/move 子树和
shared/slave 传播状态，但路径解析仍使用底层 ext4 目录；它不是完整的 VFS 挂载树或
mount namespace 实现。其余限制分别在第 4 至第 9 章中说明。

#text(size: 8.5pt, fill: rgb("536471"))[_实现追溯：_ `os/src/main.rs`、`os/src/syscall/mod.rs`、`os/src/task/`、`os/src/mm/`、`os/src/fs/`、`os/src/signal/`、`os/src/net/`]
