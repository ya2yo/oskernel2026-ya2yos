= 系统概览

Ya2yOS 是一个以 Rust 编写、面向 Linux 用户态兼容性的实验性内核。它以 TatlinOS 为基础持续演进，当前同时支持 `riscv64` 与 `loongarch64` QEMU 平台。内核采用 `no_std`、静态链接和单地址空间内核运行方式；用户程序通过 Linux 风格系统调用进入内核。Rust 的语言和内存安全模型是本项目的工程基础 #cite(<rust-book>)。

本文档以 `os/src/` 的当前代码为准，描述已经实现的职责边界和调用关系，而不把计划中的能力表述为既有功能。历史设计说明保留在上一级 Markdown 文件中，供追溯使用。

#text(size: 8.5pt, fill: rgb("536471"))[_实现追溯：_ `os/src/main.rs`、`os/Cargo.toml`]

== 设计目标

- 在两个 64 位架构上提供稳定的用户态启动、异常处理、内存隔离与系统调用入口。
- 以 Linux syscall ABI 和常见 errno 语义为兼容边界，服务于竞赛测例和常见 Unix 用户程序。
- 以 Rust 所有权、`Arc` 和受控同步原语承载内核对象生命周期，同时把硬件寄存器、页表和上下文切换封装在架构层。
- 将进程、内存、VFS、网络和驱动拆为可独立演进的模块；系统调用层只做 ABI 适配和参数编排。

== 源码组织

#table(
  columns: (6em, 1.25fr, 2fr),
  table.header([*模块*], [*主要目录*], [*责任*]),
  [引导与架构], [`os/src/main.rs`、`os/src/arch/`], [入口汇编、寄存器上下文、页表/TLB、时钟、中断与平台差异],
  [异常与 ABI], [`os/src/trap/`、`os/src/syscall/`], [trap 分发、系统调用号解析、用户参数与返回值],
  [内存], [`os/src/mm/`], [物理页、页表、VMA、ELF、缺页、COW、用户内存访问],
  [任务与信号], [`os/src/task/`、`os/src/signal/`], [进程/线程、调度、futex、等待、信号投递和返回帧],
  [文件系统], [`os/src/fs/`], [VFS、fd 表、路径、ext4、proc/dev、管道与事件对象],
  [网络与设备], [`os/src/net/`、`os/src/drivers/`], [smoltcp 封装、socket、VirtIO 块/网卡、控制台],
)

== 运行时分层

#figure(
  align(center)[
    #table(columns: 1, inset: 9pt,
      [*用户态*：ELF、libc、BusyBox、测例程序],
      [*Linux 风格 ABI*：`ecall` / trap / syscall 分发],
      [*内核服务*：任务 · 信号 · 内存 · VFS · 网络],
      [*平台抽象*：架构上下文 · 页表 · 定时器 · IRQ · 设备],
      [*硬件/虚拟硬件*：CPU · UART · VirtIO · PCI（LoongArch）],
    )
  ],
  caption: [Ya2yOS 的运行时分层。跨层交互仅通过受控的模块接口进行。],
) <runtime-layers>

分层并不意味着服务之间完全没有关联。例如缺页处理需要查询 VMA 和文件页，`read`/`write` 要经过 fd 表找到 VFS 或 socket 对象，信号可中断阻塞的等待路径。实现上以短生命周期的锁和 `Arc` 克隆来降低这种交叉依赖的风险。

== 关键对象与不变量

- *地址空间*：每个进程持有 `MemorySet`；用户地址只能经翻译和范围校验后访问。
- *线程组*：进程元数据管理线程集合、退出状态和共享资源；线程拥有各自 trap context 与调度状态。
- *文件描述符*：`FdTable` 将整数 fd 映射到统一的 `File` 抽象，普通文件、管道、socket 和事件对象使用同一访问入口。
- *锁*：任务模块定义了全局表 → 进程元数据 → 线程状态 → 资源内部锁的约束；不得跨阻塞点持有这些锁。
