#import "../diagrams.typ": flow, relation, sequence

= 进程管理

进程管理是内核把“一个正在执行的用户程序”组织成可创建、可调度、可通信、可暂停并
最终可回收对象的机制。它不仅记录 PID、地址空间和打开文件，还要维护线程之间的共享
边界、父子关系、信号与等待事件，并在用户态和内核态切换时保存可恢复的执行现场。
因此，进程的生命周期不是一次 `execve` 调用，而是一条连续链路：创建执行单元，装入用户
映像，进入就绪队列并反复运行；运行中通过线程同步和 IPC 协作；退出后暂存为 zombie，
最后由父进程等待并释放内核记录。

本章选择 PID 1 的 `INITPROC` 作为主线。它是内核启动后创建的第一个普通用户进程，既是
系统开始执行用户代码的入口，也是后续测试进程树的根。沿着它的生命线，可以把任务对象、
调度器、`clone`、`execve`、futex、IPC、信号、`exit` 和 `wait` 放进同一条因果链，而不是
把它们看成互不相关的系统调用。

== task 模块结构概览

`os/src/task/` 是进程、线程和调度的内核实现核心，`os/src/syscall/task/` 则把这些
内部操作转换成 Linux 风格用户 ABI。两者通过 TCB、`Process` 和就绪队列连接起来：
系统调用创建或改变任务，任务模块维护对象和不变量，处理器模块选择下一个执行者，
退出和等待路径则负责把运行中的对象变成可观察、可回收的 zombie。

#table(
  columns: (1.55fr, 3fr),
  table.header([*模块*], [*在生命周期中的职责*]),
  [`task/task.rs`], [定义 `TaskControlBlock`、线程状态和线程私有资源；实现 `new`、`clone_process`、`exec` 以及 `trap context`、内核栈的建立与清理。],
  [`task/process/`], [定义 `Process` 与 `ProcessMeta`，维护 PID、线程组、父子关系、进程组/会话、退出元数据和 zombie 的全局映射。],
  [`task/processor.rs`], [维护每个 Hart 的当前任务和 idle 上下文；`run_tasks` 负责记账、取任务和调用架构上下文切换。],
  [`task/scheduler/`], [通过 `ready_queue` 门面提供 `add_task`/`fetch_task`；当前可在编译期选择简化 CFS 或 FIFO RR。],
  [`task/manager.rs` 与 `task/futex.rs`], [维护 TID 到 TCB 的查找、阻塞任务计时器，以及 futex 的等待、唤醒、超时和线程退出清理。],
  [`task/mod.rs`], [提供当前任务、挂起、阻塞、停止、退出和 INITPROC 初始化等控制流入口，串起各子模块。],
  [`syscall/task/`], [实现 `clone`/`clone3`、`execve`、`exit`、`waitpid`/`waitid` 等用户可见接口，将参数校验和错误码接到内部对象。],
)

从依赖关系看，`Process` 表示线程组共享的资源，TCB 表示可调度线程，`Processor` 只负责
“当前谁在运行”，scheduler 只负责“下一个运行谁”。文件、网络、共享内存和信号并不都
位于 `task/` 目录，但它们通过 `FdTable`、地址空间、等待队列和信号投递与任务生命周期
发生联系。后文会在 INITPROC 启动测例、创建线程和进行 IPC 的具体场景中展开这些边界。

== 第一个进程

启动代码在完成内存、陷阱、文件系统和时钟初始化后，沿着
`task::init` → `fs::init` → `add_initproc` → `run_tasks` 的顺序建立第一个可调度用户任务。
`INITPROC` 是惰性初始化的 `Arc<TaskControlBlock>`：首次访问时，内核从根文件系统打开
`/initproc`，读取 ELF，并调用 `TaskControlBlock::new()`。

`new()` 同时创建三层状态：

- 一个 `Process`，分配 PID 1，建立初始 `MemorySet`、信号动作表、文件描述符表和
  `FSInfo`；文件描述符表带有标准输入、输出和错误输出。
- 一个代表初始线程的 `TaskControlBlock`（TCB），分配 TID 1、四页内核栈、内核切换
  上下文和用户 trap context。
- 一组父子关系和全局索引：初始进程没有普通父进程，但作为孤儿收养者，后来会接收
  被重新挂接的子进程。

`add_initproc()` 将 TCB 放入就绪队列，并登记到 TID 到 TCB 的映射；因此 `run_tasks()`
取到它之前，PID 1 已经是一个完整的进程对象，而不是只有一个入口地址的特殊任务。

#figure(
  flow((
    [启动：`task::init`、文件系统、时钟],
    [打开 `/initproc` 并解析 ELF],
    [创建 `Process(PID=1)` 与 `TCB(TID=1)`],
    [登记全局表并加入 ready queue],
    [首次调度，返回用户态]
  )),
  caption: [第一个进程的创建与首次运行。]
)

== 进程的两个对象：Process 与 TCB

Ya2yOS 将“可调度的线程”与“线程组共享的进程资源”分开。一个 `Process` 对应一个
Linux 风格的线程组和 PID；一个 `TaskControlBlock` 对应一个线程和 TID。TCB 通过
`Arc<Process>` 指向所属进程，所以同一线程组的多个 TCB 共享地址空间、文件表和信号动作。

#table(
  columns: (1.45fr, 3fr),
  table.header([*对象/位置*], [*职责*]),
  [`Process`\
`os/src/task/process/process.rs`], [PID、`MemorySet`、信号动作表、`FdTable`、`FSInfo`，以及父子关系、进程组/会话和退出元数据。],
  [`TaskControlBlock`\
`os/src/task/task/task.rs`], [TID、内核栈、用户 trap context、内核 `TaskContext`，以及线程私有信号、futex、凭据、计时器和调度实体。],
  [`ProcessMeta`], [活线程与子进程弱引用、父 PID、PGID/SID、退出码、事件唤醒器、stop/continue 状态和资源使用快照。],
  [`Processor` / scheduler], [每个 Hart 的当前任务和 idle 上下文；CFS/RR 就绪队列以及任务的运行时间记账。],
)

`Process` 中的地址空间和信号表通过 `ResourceSlot` 支持整体替换：`execve` 可以原子地
换入新 `MemorySet` 与信号动作表，而不必让普通文件操作持有覆盖整个进程的大锁。
`FdTable` 和 `FSInfo` 自己管理内部并发。`ProcessMeta.tasks` 与 `children` 使用弱引用，
避免关系表反过来延长已退出对象的生命周期；PID 全局映射则保留 zombie，直到等待者
真正消费退出事件。

TCB 的用户寄存器保存在用户地址空间的专用 trap 映射页，`TaskContext` 保存内核切换所需
的 callee-saved 寄存器。新任务的 `TaskContext::goto_trap_return()` 将返回地址设为
`trap_return`，因此第一次被调度时会从 trap context 恢复用户寄存器，而不是直接跳入
Rust 函数。`TidHandle` 的分配器从 1 开始，当前 `Drop` 不归还 PID/TID：否则 zombie
尚未被父进程等待时重用 ID，会覆盖全局 PID 映射。

== 第一次调度：从 Ready 到 Running

每个 Hart 的 `Processor` 保存 `current` 和 `idle_task_cx`。`run_tasks()` 检查定时器、
futex 超时和阻塞任务计时器，记账后把可再次运行的当前任务放回就绪队列，再取出一个
`Ready` 任务，标记为 `Running`，并调用架构相关的 `switch()` 切换到它的 `TaskContext`。
于是 PID 1 的第一次用户态执行可以概括为：

`Ready → Running → trap_return → /initproc 用户入口`。

任务状态的含义如下：

#table(
  columns: (1.3fr, 3fr),
  table.header([*状态*], [*生命周期中的含义*]),
  [`Ready`], [已具备运行条件，等待调度器选取。],
  [`Running`], [被某个 Hart 的 `Processor.current` 持有并执行。],
  [`Blocked`], [等待 futex、I/O 或异步事件；唤醒后回到 `Ready`。],
  [`Stopped`], [被 stop 类信号暂停，等待恢复。],
  [`VforkBlocked`], [`CLONE_VFORK` 的父线程等待子线程 `exec` 或退出。],
  [`Zombie`], [线程已退出；进程资源和 PID 记录是否最终删除取决于等待回收。],
)

当前默认调度器是编译期选择的简化 CFS：所有 Hart 共享按 `vruntime` 排序的
`BinaryHeap<CfsEntry>`，首次入队的新任务放在共享 `min_vruntime` 坐标上；`scheduler-rr`
则提供全局 FIFO 队列。两者都通过 `ready_queue::add_task()` / `fetch_task()` 服务
创建和唤醒路径，并按 CPU affinity 过滤当前 Hart 不可运行的任务。时钟中断（当前 100Hz）、
主动 yield、阻塞和唤醒都会回到调度循环。

这不是完整 Linux 调度器：调度策略由 Cargo feature 决定，不能通过用户 ABI 运行时切换；
尚无完整实时调度类、调度组、work stealing 和负载均衡。已有远程入队和空闲 Hart 通知，
但不能据此描述为完整的多核迁移机制。

=== CFS 如何决定下一个任务

对 INITPROC 来说，“进入就绪队列”并不意味着立即运行；它必须和 shell、测试脚本以及
测试脚本创建的子任务竞争处理器时间。默认的 `scheduler-cfs` 用每个任务的
`sched_entity.vruntime` 表示其相对运行进度：任务实际运行一段时间后，内核按照
`nice` 权重折算并增加 `vruntime`，运行得相对少的任务在下一轮更有机会被选中。就绪队列
内部是按 `vruntime` 排序的 `BinaryHeap<CfsEntry>`，共享的 `min_vruntime` 为新任务提供
初始坐标，避免刚创建的任务因绝对时间落后或领先而获得不合理的优势。

调度循环由 `Processor::run_tasks()` 串起。时钟中断、主动 `yield`、任务阻塞或被唤醒时，
内核先为当前任务记账并更新其调度实体，再通过 `ready_queue::add_task()` 入队；随后
`fetch_task(hartid)` 从全局堆中挑选 `vruntime` 最小且满足 CPU affinity 的 `Ready` 任务，
将其标记为 `Running`，最后由 `switch()` 切换到该任务的 `TaskContext`。所以 INITPROC
第一次运行和后续测试任务的运行遵循同一条路径：

`Ready → CFS 选择 → Running → 被抢占/阻塞 → 重新入队或等待唤醒`。

当 INITPROC `fork`/`clone` 测试任务时，子任务也必须经过这条路径；创建系统调用返回后，
父任务不会直接调用子任务的用户入口，而是由子任务以 `Ready` 状态加入队列，等待 CFS
在合适的时机选择。测试程序使用 futex、pipe 或 socket 阻塞时，它从可运行集合中移出；
对端数据到达、futex 唤醒或定时器到期后才重新进入队列。这正是调度策略与后文线程创建、
IPC 生命周期的连接点。

`make SCHEDULER=rr` 可在编译时改用 FIFO RR：接口仍是 `add_task()` / `fetch_task()`，
但不再比较 `vruntime`，而是按入队顺序轮转。因此本文后文只使用“进入 ready queue”
这一稳定抽象描述 clone、阻塞和唤醒，而把 CFS 的具体选择策略限定为当前默认实现。

== 第一个进程如何产生后继者：clone 与 clone3

当 `/initproc` 或其后代执行 `clone` 时，`sys_clone()` 解析退出信号和 `CloneFlags`，
检查互斥组合，再调用 `TaskControlBlock::clone_process()`。创建过程分为两阶段：先读取
父 TCB/Process 状态并决定资源共享或复制策略，再在不持有父任务锁时构造子 TCB、trap
context、关系表和全局 TID 索引，最后把子任务加入 ready queue。

#table(
  columns: (1.65fr, 3fr),
  table.header([*标志*], [*创建结果*]),
  [`CLONE_VM`], [共享 `MemorySet`；若没有 `CLONE_THREAD`，仍创建新的 `Process`/PID。非 `CLONE_VM` 路径复制地址空间。],
  [`CLONE_THREAD`], [复用父 `Process`、PID、父 PID 和进程级资源，只增加 TID；不创建新的 `/proc` 进程目录。],
  [`CLONE_FS` / `CLONE_FILES`], [分别共享 `FSInfo` / `FdTable`；未设置时用 `from_another()` 复制。],
  [`CLONE_SIGHAND`], [共享信号动作表；未设置时复制，`CLONE_CLEAR_SIGHAND` 创建空表。],
  [`CLONE_SETTLS`], [把 TLS 写入子 trap context 的线程指针寄存器。],
  [`SETTID` / `CHILD_CLEARTID`], [向用户地址写 TID；退出或 exec 前清零并 futex 唤醒。],
  [`CLONE_VFORK`], [父线程进入 `VforkBlocked`，子线程 exec 或退出时唤醒父线程。],
)

子 trap context 从父线程复制，但子路径把返回值寄存器设为 0；用户指定非零栈时覆盖
`sp`。普通子进程继承 PGID/SID 并登记为父进程的 child，线程型 clone 则继续使用同一
Process。`sys_clone3()` 当前是适配层：读取并校验 `clone_args` 后转换到 legacy
`sys_clone()`；`set_tid`、pidfd、cgroup 等扩展字段仍返回不支持或参数错误，不能视为
完整 `clone3` 实现。

== execve：同一个进程换一套用户世界

`clone` 常用于创建执行单元，`execve` 则不创建 PID/TID，而是让当前线程组中的调用线程
把旧用户映像替换为新映像。`sys_execve()` 先从用户空间安全复制路径、`argv` 和 `envp`，
按 `FSInfo.cwd` 归一化路径，检查执行权限和写打开冲突，再读取 ELF 头、程序头和可加载
内容。非 ELF 文件会继续尝试 shebang；解释器和脚本路径被重建到 `argv` 前部，镜像中
`/bin/sh`、`/bin/busybox` 可按兼容规则转到 `/musl/busybox`。

`MemorySetInner::from_elf()` 映射 `PT_LOAD` 段并按 ELF 标志设置用户权限；存在 `PT_INTERP`
时还映射动态解释器，把入口设为解释器入口。加载器建立 brk、栈 guard page，并生成
`AT_PHDR`、`AT_PHENT`、`AT_PHNUM`、`AT_ENTRY`、`AT_BASE`、`AT_PAGESZ` 等 auxv。共享库
解析和重定位仍由用户态解释器完成。

`TaskControlBlock::exec()` 的提交顺序很重要：先处理旧地址空间中的 `clear_child_tid`，
再替换 `MemorySet` 与信号动作表，重建用户栈和 trap 资源，关闭 `close_on_exec` 描述符，
清空线程 pending/mask 信号，并在新栈写入 `argc`、字符串、指针数组、auxv 和随机区。
调用线程的 `comm` 更新为 `argv[0]` 的末级名称；旧映像的 rseq 注册也被清除。若父线程
因 `CLONE_VFORK` 等待，exec 完成后会唤醒父线程。此时 PID/TID 和进程关系仍保持不变，
改变的只是该线程看到的用户映像。

== rseq：线程级可重启序列

`rseq(2)` 用于用户态实现短小的、与当前 CPU 相关的可重启临界区。注册入口位于
`os/src/syscall/task/rseq.rs`，状态保存在 TCB 的 `TaskControlBlockInner.rseq` 中，
因此它是线程私有状态而不是 `Process` 资源。当前实现支持 classic 32 字节 ABI：用户
提供按 32 字节对齐的区域，内核校验地址、长度和签名后记录注册；在返回用户态前写入
当前 Hart 编号，并检查 `rseq_cs` 是否仍指向未提交的临界区。

若任务在临界区中被抢占、迁移或发生异常，内核清除描述符并把 trap 返回地址改为用户
指定的 `abort_ip`，让 libc 重新执行该序列；用户区域无效时停止继续访问并按错误路径
交付 `SIGSEGV`。当前没有 NUMA 拓扑和扩展 ABI，因此 `node_id` 与 `mm_cid` 写为 0。
`CLONE_VM` 创建的子线程不继承父线程指向旧 TLS 的 rseq 注册，普通 fork 路径可继承；
`execve` 替换用户映像时清除旧注册，线程退出时丢弃 TCB 中的状态。调度器在任务切换
和返回用户态前设置 pending 标记，使 rseq 的 CPU 发布与上下文切换相连。

== seccomp：系统调用入口的线程过滤

`seccomp` 是在系统调用真正分派前执行的安全策略。Ya2yOS 在
`os/src/task/seccomp.rs` 保存每个 TCB 的 `SeccompState`，系统调用分发器调用
`task.seccomp_action(id)` 决定是否继续执行。严格模式只允许少数安全系统调用，其他
调用由内核注入 `SIGKILL`；过滤模式加载最多 4096 条 classic BPF 指令，程序读取
`seccomp_data.nr`，可返回允许、拒绝（通常为 `EPERM`）或 `SIGKILL` 等动作。

`sys_seccomp()` 和 `prctl(PR_SET_SECCOMP)` 负责校验 flags、用户过滤器和单调安装策略；
过滤器一旦安装不能由普通路径撤销或放宽。seccomp 状态按 clone 语义随线程创建继承，
`execve` 不会借此绕过已经安装的限制，线程退出时随 TCB 清理。测试程序可以先安装只
允许所需 ABI 的过滤器，再执行文件、网络或 IPC 系统调用；违规调用在入口被拦截，不会
进入具体子系统，严格模式则直接结束测试进程，父进程仍可通过 `waitpid` 观察退出状态。

== INITPROC 如何执行测例

内核完成首次调度后，PID 1 并不是一个交互式 shell，而是用户态的
`user/src/bin/initproc.rs`。用户运行库的 `_start()` 先初始化 32 KiB 堆，再调用弱符号
`main()`，最后把返回值传给 `exit()`；因此 INITPROC 的用户入口仍然遵循普通用户程序的
启动与退出 ABI。

当前工作树的 `main()` 默认调用 `test_final_2026()`。它依次调用两次
`run_final_testsuit()`：先在 `glibc` 根目录执行 `cagent_testcode.sh`，再执行
`buildstorm_testcode.sh`，全部完成后调用 `shutdown()`。备用的 `test_pre()` 会按顺序运行
musl/glibc 下的 basic、busybox、lua、iperf、netperf、cyclictest、libctest、iozone、
lmbench、libcbench 和 LTP；`test()` 则直接运行网络、文件、信号、rseq、时间和内存回归。
交互 shell 路径也保留着，但当前没有被 `main()` 调用。

每个测试套的启动都体现同一条进程生命周期：INITPROC 调用 `fork()`，子进程先
`chdir()` 到测试根目录，再调用 `execve()`；父进程用 `waitpid()` 等待并读取退出码。
`run_final_testsuit()` 使用 `[/bin/bash, script]`，普通 `run_testsuit()` 使用
`[busybox, sh, script]`。脚本本身还会通过 shell 的后台执行（`&`）产生更多子进程，
所以内核看到的是 INITPROC → 测试脚本 → 测例程序的多级父子树，而不是 INITPROC 直接
加载每一个测试 ELF。测试套结束后，INITPROC 用 `kill_processes(-1, SIGKILL)` 清理残留
进程，再循环 `wait()` 回收它们，避免一个失败或超时的后台任务污染下一个测试。

`execve` 的失败也遵循同样的生命周期：子进程打印错误并以 127 退出，父进程仍然
`waitpid()` 后继续后续测试。脚本为非 ELF 时，内核会在 `sys_execve()` 中解析 shebang，
重建解释器参数；因此测试脚本的解释器选择最终仍由内核的 exec 路径和镜像中的文件布局
共同决定。

#figure(
  sequence((
    ([INITPROC], [fork], [测试套子进程]),
    ([测试套子进程], [execve 脚本解释器], [shell]),
    ([shell], [fork/exec、`&`、wait], [测试程序]),
    ([INITPROC], [kill 残留并 wait], [测试树回收])
  )),
  caption: [INITPROC 启动测试套及回收测试进程树。]
)

== 测例中的线程与进程协作

测试程序使用 libc/pthread 或直接调用 Linux 风格 ABI 创建执行单元。`clone()` 通过
低 8 位解析退出信号，再校验 `CLONE_THREAD` 必须同时带有 `CLONE_VM` 和 `CLONE_SIGHAND`。
`TaskControlBlock::clone_process()` 先快照父线程状态，再决定地址空间、文件表、文件系统
上下文和信号表是共享还是复制，最后为子线程建立独立内核栈、trap context、TLS 和 TID。
因此 pthread 线程通常共享同一个 `Process`、PID、地址空间和 fd 表，但拥有独立栈、寄存器、
线程信号状态、rseq 和 futex 等线程私有状态；子线程从父 trap context 复制执行现场，
但用户态看到的返回值为 0。

线程同步通常从共享地址空间中的 futex word 开始。`FUTEX_WAIT` 在内核重新检查用户值后，
将当前 TCB 置为 `Blocked` 并登记 waiter；`FUTEX_WAKE`、requeue、信号或超时把 waiter
移出等待队列并恢复为 `Ready`。线程退出时，`clear_child_tid` 会清零用户地址并 futex
唤醒等待者；robust list 则处理持锁线程异常退出留下的互斥量。这样，pthread join、
条件变量和取消操作最终都能回到“阻塞—唤醒—再次调度”的内核状态机。

`CLONE_VFORK` 是另一种显式协作：父线程变为 `VforkBlocked`，直到子线程完成
`execve` 或退出；普通 `fork` 则不阻塞父线程，父子通过调度和 `waitpid` 独立推进。

== 测例中的 IPC：共享数据与异步控制

进程间通信并非单一路径，而是由 fd、共享页、等待队列和信号共同组成：

#table(
  columns: (1.55fr, 3fr),
  table.header([*IPC 方式*], [*从创建到销毁的实现闭环*]),
  [`pipe` / FIFO], [`sys_pipe2()` 创建两个 fd；读写端共享 `Arc<Mutex<PipeRingBuffer>>`。读写阻塞时进入等待队列，端点 `Drop` 更新读写端计数并唤醒对端；没有读端时写入返回 `EPIPE` 并可产生 `SIGPIPE`。`splice`/`tee` 可在 pipe 与页缓存之间转移或复制缓冲区。],
  [`socketpair` / socket], [`sys_socketpair()` 创建 Unix 成对端点，fd 表决定端点如何随 fork/clone 继承；TCP、UDP 和 Unix socket 经 `SocketOps` 分发 connect、listen、accept、send、recv 和 shutdown，关闭时释放端点并唤醒等待者。],
  [`System V message queue`], [测例可通过 `msgget`、`msgsnd`、`msgrcv` 和 `msgctl` 创建、发送、按类型接收、查询/调整队列并 `IPC_RMID` 删除；阻塞接收者由队列事件唤醒，队列删除返回 `EIDRM`。],
  [`共享内存`], [`shmget` 创建全局 segment，`shmat` 将同一组物理页映射进当前进程，多个进程可直接读写；`shmdt` 解除当前映射，`IPC_RMID` 删除管理记录。生命周期由 segment 引用和各进程地址空间映射共同决定。],
  [`futex / signal`], [`futex` 为共享内存上的同步控制面；信号则从 pending 队列经用户态 signal frame 投递 handler，再由 `sigreturn` 恢复上下文。`SIGCHLD` 通知父进程 wait，`SIGKILL` 驱动 exit_group，`SIGPIPE` 报告 pipe 读端消失。`SIGIO` 可报告异步 pipe 就绪。],
)

以消息队列测例为例，用户先用 `msgget(IPC_PRIVATE, IPC_CREAT)` 获得队列，再用
`msgsnd` 放入不同类型的消息，用 `msgrcv` 按类型选择或 `MSG_COPY` 观察队列，最后用
`msgctl(IPC_RMID)` 删除资源。这个过程与匿名 pipe 的共享 ring buffer 不同，但都遵循
“内核对象创建 → 数据传递或阻塞等待 → 唤醒对端 → 显式关闭/删除”的生命周期。

IPC 与 clone 的关系由资源共享标志决定：`CLONE_FILES` 使线程或子任务直接共用 fd 表，
未设置时复制 fd 表但仍继承打开文件对象的语义；`CLONE_VM` 让 futex 私有键或共享键
能够分别映射到同一进程地址空间或同一物理页；信号投递则根据线程、线程组或进程组选择
可接收者。于是一个典型测试可按如下顺序运行：父进程建立 pipe/socket/shm，fork 或
clone 子任务，子任务 exec 测试程序，双方在 IPC 上阻塞与唤醒，最后关闭 fd、detach
共享内存并 wait 回收子进程。

== 退出、僵尸与等待

`sys_exit()` 只终止调用线程；`sys_exit_group()` 先设置一次线程组退出码，向其他成员
发送 `SIGKILL` 并唤醒阻塞线程，使它们分别经过退出路径。退出当前线程时依次完成：

1. 执行 `CLONE_CHILD_CLEARTID` 清零和 futex 唤醒，并处理 robust futex；
2. 唤醒 `CLONE_VFORK` 的父线程，移除 trap context 映射；
3. 从 futex 等待队列和 `ProcessMeta.tasks` 清除自己，标记线程为 `Zombie`；
4. 若仍有兄弟线程，唤醒必要的阻塞者；若这是最后一个线程，结束进程级生命周期。

最后一个线程退出时，内核冻结资源使用量，清理 trap area、地址空间数据、文件描述符、
文件系统上下文、POSIX 文件锁和文件租约，然后调用 `exit_and_reparent()`。仍存活的
子进程优先改挂到最近的存活 child subreaper，没有时改挂到 PID 1；退出信号通知新父进程，
并唤醒 `child_exit_event`。因此“退出”并不等于“PID 记录立即消失”：Process 会保留为
zombie，让父进程能够读取退出状态和资源使用量。

#figure(
  sequence((
    ([用户态 `exit`], [清理线程私有资源], [线程 `Zombie`]),
    ([最后线程], [清理 Process 资源并 reparent], [通知父进程]),
    ([父进程 wait], [读取状态和资源使用量], [回收 PID 记录])
  )),
  caption: [退出、僵尸和回收的先后关系。]
)

== 父进程 wait：生命周期的真正终点

`sys_waitpid()` 按 PID、进程组或任意子进程筛选可等待对象，支持 `WNOHANG`、`WNOWAIT`、
`WUNTRACED`/`WSTOPPED`、`__WALL` 和 `__WCLONE`；`sys_waitid()` 使用相同筛选机制，
并以 `siginfo_t` 报告 `WEXITED`、`WSTOPPED`、`WCONTINUED`。没有匹配子进程返回 `ECHILD`；
允许阻塞时，父进程在 `ProcessMeta.child_exit_event` 上注册 waker，退出、stop 或
continue 事件使其重新扫描。

成功消费退出事件且没有 `WNOWAIT` 时，父进程累计子进程资源使用量，从 `children` 删除
弱引用，并调用 `Process::remove_from_global_map()`。这一步才使 zombie 的进程记录从全局
PID 映射消失；`WNOWAIT` 只观察状态，不完成这次回收。由此，第一个进程的完整闭环为：

#figure(
  flow((
    [`INITPROC` 惰性创建],
    [`PID 1 / TID 1` 加入就绪队列],
    [`run_tasks` 首次进入 `/initproc`],
    [`clone` 派生线程或子进程],
    [`execve` 替换用户映像],
    [`exit` / `exit_group` 成为 zombie],
    [`waitpid` / `waitid` 回收记录]
  )),
  caption: [以第一个进程为主线的生命周期闭环。]
)

== 实现边界与追溯

#table(
  columns: (1.55fr, 3fr),
  table.header([*边界*], [*当前实现*]),
  [PID/TID 生命周期], [`TidHandle::Drop` 当前不归还 ID；为保护 zombie 等待期间的映射，ID 单调消耗。],
  [调度], [默认简化 CFS；`make SCHEDULER=rr` 编译 FIFO RR，不能在运行时切换，尚无完整实时类和负载均衡。],
  [clone3], [仅适配已支持的 `clone_args`；`set_tid`、pidfd、cgroup 等扩展未实现。],
  [ELF 动态加载], [内核映射 `PT_INTERP` 并提供 auxv；共享库重定位由用户态解释器完成。],
  [多核], [有共享就绪队列、远程入队和空闲 Hart 通知，但尚无完整主动迁移、work stealing 或负载均衡。],
)

关键实现位置：

- 启动与初始进程：`os/src/main.rs`、`os/src/task/mod.rs` 的 `INITPROC`/`add_initproc`；
- 对象与生命周期：`os/src/task/task/task.rs`、`os/src/task/process/process.rs`；
- 调度与切换：`os/src/task/processor.rs`、`os/src/task/scheduler/`、`os/src/task/switch.rs`；
- 用户 ABI：`os/src/syscall/task/clone.rs`、`clone3.rs`、`execve.rs`、`exit.rs`、`wait.rs`；
- ELF 与地址空间：`os/src/mm/memory_set/elf_loader.rs`；
- 线程控制面：`os/src/task/rseq.rs`、`os/src/task/seccomp.rs`、`os/src/syscall/task/rseq.rs`、`os/src/syscall/sys/prctl.rs`。
