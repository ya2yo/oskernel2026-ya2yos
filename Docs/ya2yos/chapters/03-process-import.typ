#import "../diagrams.typ": flow, relation, sequence

= 进程管理

进程管理子系统是 Ya2yOS 内核的核心模块，代码位于 `os/src/task/` 目录下，包含以下源文件：

#table(
  columns: 2,
  table.header([*文件*], [*职责*]),
  [`mod.rs`], [模块入口：调度控制流入口函数（suspend/block/exit/stop）、INITPROC 初始化、`add_initproc()`],
  [`process/process.rs`], [`Process`、`ProcessInner`、`ProcessMeta`、`ProcessUsage` 的定义与实现],
  [`task/task.rs`], [`TaskControlBlock`（TCB）、`TaskControlBlockInner` 的定义与核心操作（new/clone/exec）],
  [`manager.rs`], [全局 `TaskManager`：`ready_queue`（就绪队列）、`tid_to_task`（TID→TCB 映射）、futex 任务唤醒],
  [`processor.rs`], [每 Hart 的 `Processor` 结构体、`run_tasks()` 调度主循环、`schedule()`],
  [`switch.rs`], [`__switch`/`__abandon` 汇编声明的 Rust 包装：上下文切换的安全入口],
  [`tid.rs`], [`TidHandle` RAII 句柄、全局 `IdAllocator` 实例],
  [`kernel_stack.rs`], [`KernelStackOnHeap`：内核栈的物理页分配与地址计算],
  [`futex.rs`], [futex 系统调用实现：快速用户空间互斥量（含 robust futex 死者恢复）],
  [`aux.rs`], [ELF 辅助向量（auxv）的类型定义（`AuxType` 枚举、`Aux` 结构体）],
  [`sysinfo.rs`], [`Sysinfo` 结构体：系统信息快照（uptime、内存统计、进程数）],
  [`future/`], [异步运行时支持（poll/interrupt 机制），本章不展开],
)


---

== 进程与线程模型


Ya2yOS 采用 Linux 风格的 *轻量级进程* 模型。在此模型中：

- *Process*（进程/线程组）：代表一个独立的资源集合，包括虚拟地址空间（`MemorySet`）、文件描述符表（`FdTable`）、信号处理表（`SigTable`）、文件系统信息（`FSInfo`）等。每个 Process 有一个唯一的 `pid`。其实现位于 `os/src/task/process/process.rs`。
- *TaskControlBlock（TCB）*（任务控制块）：代表一个可调度的执行单元（即线程）。每个 TCB 有一个唯一的 `tid`，归属于某个 Process。同一 Process 下的所有 TCB 共享地址空间和文件描述符等资源。其实现位于 `os/src/task/task/task.rs`。

这种设计使得多线程程序的实现变得自然——同一进程的多个线程共享地址空间和文件描述符，但各自拥有独立的寄存器上下文和内核栈。

`Process` 与 `TaskControlBlock` 的核心区别在于：*Process 管理资源，TCB 管理执行*。Process 记录进程拥有哪些内存页、打开了哪些文件、注册了哪些信号处理器；TCB 记录线程当前在 CPU 的什么位置执行（sepc）、内核栈指针在哪（sp）、是否在等待 futex（futex_key/futex_pa）、哪些信号被屏蔽（sig_mask）。当 Linux 的 `pthread_create()` 创建新线程时，仅新增 TCB 并共享 Process；当 `fork()` 创建新进程时，同时复制 Process（COW）和 TCB。

=== 核心数据结构与 GRASP 设计分析


以下展示了进程管理中的核心数据结构（定义于 `process/process.rs` 和 `task/task.rs`）。与 TatlinOS 相比，本内核遵循 *GRASP 设计模式*中的多项原则来提升架构质量。

*源文件归属*：`Process`、`ProcessInner`、`ProcessMeta`、`ProcessUsage` 定义于 `os/src/task/process/process.rs`；`TaskControlBlock`、`TaskControlBlockInner`、`TaskStatus`、`RobustListHead`、`CapabilitySets` 定义于 `os/src/task/task/task.rs`。

*Information Expert（信息专家）*——数据与操作同驻。`Process` 和 `TaskControlBlock` 各自持有自己所管理的全部状态数据，而非将数据散布在全局变量中。当一个操作需要访问内存空间时，它调用 `ProcessInner::get_locked_memory_set_read()`；当需要修改线程状态时，它获取 `TaskControlBlockInner` 的锁——操作逻辑与数据紧密绑定在同一结构体内。

*High Cohesion（高内聚）*——`ProcessInner` 将 `memory_set`、`sig_table`、`fd_table`、`fs_info` 这四种"进程级共享资源"集中管理；`TaskControlBlockInner` 则将 `trap_cx_ppn`、`task_cx`、`task_status`、`futex_key`、`sig_mask` 等"线程私有状态"聚合在一起；`ProcessMeta` 专注于父子关系、进程组等"进程间关系元数据"。三个结构体各司其职，避免了将共享资源和私有状态混装在同一结构体中的低内聚问题。

*Low Coupling（低耦合）*——相比于 TatlinOS 将 `FdTable` 等资源直接放在 TCB 内部的做法，本设计通过两方面降低耦合：(1) 将锁下放到 `FdTable` 内部自行管理，`ProcessInner` 的锁只需保护自身结构的替换（如 exec 替换地址空间），而资源的内部并发访问由各自的内部锁负责；(2) TCB 通过 `Arc<Process>` 共享进程资源，线程退出时仅释放自己的引用，不影响仍存活的兄弟线程。这种基于引用计数的所有权松耦合是并发内核的核心设计模式。

```rust
// Process：进程/线程组
pub struct Process {
    pub inner: Mutex<ProcessInner>,  // 可变的资源集合
    pub pid: usize,                  // 进程ID
    pub meta: Mutex<ProcessMeta>,    // 子进程列表、父进程ID等元数据
}

pub struct ProcessInner {
    pub memory_set: Arc<RwLock<MemorySet>>,  // 虚拟地址空间
    pub sig_table: Arc<Mutex<SigTable>>,      // 信号处理表
    pub fd_table: Arc<FdTable>,               // 文件描述符表
    pub fs_info: Arc<FSInfo>,                 // 当前工作目录、umask等
    pub personality: u32,                     // PER_LINUX = 0
}
```

*`ProcessInner` 各字段解析*：

- `memory_set: Arc<RwLock<MemorySet>>` — 进程的虚拟地址空间。`RwLock` 允许读-读并发（如多个线程同时处理页面错误时的页表遍历），写操作（如 `mmap`/`munmap`）需独占锁。`Arc` 使得 `CLONE_VM` 的线程可以零拷贝共享地址空间。
- `sig_table: Arc<Mutex<SigTable>>` — 信号处理表，包含 64 个信号的 `KSigAction`（处理器函数指针/标志/掩码）以及进程退出码 `exit_code` 和 `SIGNAL_GROUP_EXIT` 标志。`Arc` 实现了 `CLONE_SIGHAND` 时的信号处理器共享。
- `fd_table: Arc<FdTable>` — 文件描述符表，每个条目指向一个 `FileDescriptor`（含 inode、偏移量、`O_CLOEXEC` 标志等）。自身管理内部锁，`ProcessInner` 的锁无需覆盖文件 I/O 操作的并发。
- `fs_info: Arc<FSInfo>` — 文件系统上下文：当前工作目录（cwd）、可执行文件路径（exe）、umask。`CLONE_FS` 时共享此结构。
- `personality: u32` — 进程执行域（`personality(2)` 系统调用），默认为 `PER_LINUX = 0`。

```rust
// TaskControlBlock：线程/任务
pub struct TaskControlBlock {
    tid: TidHandle,                     // 线程ID句柄
    kernel_stack: KernelStackOnHeap,    // 内核栈
    pub process: Arc<Process>,          // 所属进程
    pub interrupted: AtomicBool,        // 异步中断标记（用于polling）
    pub interrupt_waker: AtomicWaker,   // 异步唤醒器
    inner: Mutex<TaskControlBlockInner>, // 线程私有可变状态
}

pub struct TaskControlBlockInner {
    trap_cx_ppn: PhysPageNum,       // Trap上下文物理页
    pub trap_cx_bottom: usize,      // Trap上下文虚拟地址基地址
    pub task_cx: TaskContext,       // 任务上下文（callee-saved寄存器）
    pub task_status: TaskStatus,    // 任务状态
    pub time_data: TimeData,        // CPU时间统计
    pub user_heappoint: usize,      // 堆顶指针（brk）
    pub user_heapbottom: usize,     // 堆底指针
    pub clear_child_tid: usize,     // CLONE_CHILD_CLEARTID目标地址
    pub vfork_wait_child: usize,    // vfork等待的子线程tid
    pub sig_mask: SigSet,           // 信号屏蔽字
    pub sig_pending: SigSet,        // 待处理信号集
    pub timer: Arc<Timer>,          // 间隔/单次计时器
    pub robust_list: RobustListHead,// robust futex链表头
    pub futex_key: usize,           // futex等待键值
    pub futex_pa: usize,            // futex等待物理地址
    pub futex_timedout: bool,       // futex wait 因超时唤醒
    pub sig_eintr: bool,            // 信号已交付，应返回EINTR
    pub sigtimedwait_timedout: bool,// sigtimedwait 超时标记
    pub nice: i32,                  // nice值 (-20..19, 默认0)
    // ... 另有 POSIX credentials (uid/gid/capabilities) 和 pdeath_signal 字段
}
```

*`TaskControlBlockInner` 各字段解析*（定义于 `task/task.rs`）：

- `trap_cx_ppn` / `trap_cx_bottom` — Trap 上下文的物理页号和用户态虚拟地址。当线程因系统调用/异常/中断陷入内核时，CPU 寄存器被硬件保存到 `trap_cx_ppn` 指向的物理页；返回用户态时从此恢复。`trap_cx()` 方法通过 `ppn.as_mut()` 返回 `&'static mut TrapContext`。`trap_cx_bottom` 记录了 Trap 页在用户地址空间的起始虚拟地址，用于 exit 时回收该区域。
- `task_cx: TaskContext` — 内核态上下文切换的 callee-saved 寄存器快照（ra, sp, s0-s11）。调度器执行 `__switch` 时将当前任务的寄存器保存到此，从下一任务的此结构恢复。
- `task_status: TaskStatus` — 线程的调度状态（见 2.1.2 节）。
- `time_data: TimeData` — CPU 时间统计：`utime`（用户态）、`stime`（内核态）、`cutime`/`cstime`（已回收子进程累计）、`cmaxrss`（子进程最大 RSS）。
- `user_heappoint` / `user_heapbottom` — brk 堆边界。`growproc()` 通过 `MemorySet::grow()` 懒分配扩展堆区。
- `clear_child_tid: usize` — 若通过 `CLONE_CHILD_CLEARTID` 创建，线程退出时内核将 `*clear_child_tid` 清零并 `futex_wake_up` 唤醒等待者，用于 `pthread_join` 同步。
- `vfork_wait_child: usize` — VFORK 阻塞时记录等待的子线程 tid。子线程 exec/exit 时遍历父进程 tasks 找到匹配 tid 并唤醒。
- `sig_mask` / `sig_pending` — 信号屏蔽位图和挂起位图。返回用户态时 `trap_handler` 检查 `sig_pending & !sig_mask` 决定是否交付信号。
- `timer: Arc<Timer>` — POSIX 间隔/单次计时器（`setitimer`/`getitimer`）。`CLONE_THREAD` 时共享（Arc::clone），非线程时独立创建。
- `robust_list: RobustListHead` — robust futex 链表头（详见 2.5.3 节）。线程退出时遍历此链表标记 `FUTEX_OWNER_DIED` 并唤醒等待者，防止持锁线程意外终止导致死锁。
- `futex_key` / `futex_pa` — futex 等待标识。`futex_key` 是全局原子递增的唯一键（从 `FUTEX_KEY_COUNTER` 获取，1 起始，0 表示"已清理"）；`futex_pa` 是等待字的物理地址。两者用于在 `FUTEX_QUEUE_BITMAP` 中定位和移除等待条目。
- `futex_timedout: bool` — futex 等待超时标记。`handle_timer()` 在定时器触发时设为 true，`futex_wait_bitset` 醒来后检查返回 `ETIMEDOUT`。
- `sig_eintr: bool` — 信号交付标记。信号处理器执行完毕后 `sigreturn` 回内核时设置，可中断 syscall 醒来后检查决定是否返回 `EINTR`。
- `sigtimedwait_timedout: bool` — `sigtimedwait(2)` 超时标记，由 `handle_sigtimedwait_timer()` 设置并通过 `task.interrupt()` 唤醒 `block_on` 中的 `poll_fn`。
- `nice: i32` — 调度优先级（-20..19，默认 0）。当前 FIFO 调度器未使用，为未来 CFS 预留。

#figure(relation(([*Process*\资源：MemorySet、FdTable、FSInfo], [*TaskControlBlock*\执行：Context、KernelStack、Status], [*TaskManager / Processor*\调度：ready queue、current task])), caption: [进程管理核心对象关系。])


=== 任务状态


TCB 可以处于以下状态之一：

#table(
  columns: 2,
  table.header([*状态*], [*含义*]),
  [`Ready`], [就绪，等待调度执行],
  [`Running`], [当前正在某个 Hart 上执行（由 Processor 跟踪）],
  [`Blocked`], [阻塞，等待某个事件（futex、pipe、信号等）],
  [`VforkBlocked`], [因 vfork 而被阻塞，等待子进程 exec 或 exit],
  [`Zombie`], [僵尸状态，线程已退出但尚未被父进程回收],
  [`Stopped`], [被信号停止（SIGSTOP/SIGTSTP 等）],
)


#figure(sequence((( [用户态], [syscall / trap 进入内核], [TaskControlBlock] ), ( [TaskControlBlock], [更新状态并进入 ready/block/exit 路径], [TaskManager] ), ( [TaskManager], [选择下一 ready 任务并切换上下文], [Processor] ))), caption: [任务调度主序列。])


== 任务管理器与调度器


`os/src/task/mod.rs` 是进程管理子系统的顶层入口，它不仅做模块声明，还定义了调度控制流的六个核心入口函数以及 initproc 的初始化逻辑。这些函数是调度器与外部世界（时钟中断、系统调用、信号）的接口层。

`mod.rs` 定义的关键函数：

#table(
  columns: 3,
  table.header([*函数*], [*触发场景*], [*行为*]),
  [`suspend_current_and_run_next()`], [时钟中断（时间片耗尽）], [将当前任务放回就绪队列末尾 → `schedule()` 切到下一任务。先检查进程是否已退出，若已退出则走 `exit_current_and_run_next`],
  [`block_current_and_run_next()`], [futex/Pipe/Socket 等阻塞等待], [从 `Processor.current` 取走当前任务，标记 `Blocked`，不放入就绪队列 → `schedule()` 切走],
  [`stop_current_and_run_next()`], [SIGSTOP/SIGTSTP 等停止信号], [类似 block，但标记 `Stopped` 而非 `Blocked`（语义区分：Stopped 可被 SIGCONT 恢复）],
  [`exit_current_and_run_next()`], [sys_exit / exit_group 的最后一步], [完整的线程退出 + 进程级回收 + `abandon(tid)` 移交 IDLE（见 2.5 节）],
  [`exit_current_group_and_run_next()`], [sys_exit_group], [向所有兄弟线程发 SIGKILL，唤醒阻塞的兄弟线程，最后调用 `exit_current_and_run_next`],
  [`schedule_blocked_current(task_cx_ptr)`], [等待队列内部场景], [当前任务已被等待队列标记为 Blocked，只负责切回调度器（不再重复标记）],
)


*`suspend_current_and_run_next` 的微妙逻辑*：它不是简单地放回就绪队列。它首先检查进程的 `sigtable.is_exited()`——若 `exit_group` 已设置 `SIGNAL_GROUP_EXIT` 标志，`suspend` 路径会自动转入 `exit_current_and_run_next`，确保被 SIGKILL 的线程不会在时间片耗尽后继续运行。若进程未退出，则仅当任务不是 `VforkBlocked` 时才放回就绪队列——vfork 父进程的阻塞状态必须保持到子进程 exec/exit。

*`block_current_and_run_next` vs `schedule_blocked_current`*：前者的调用者（如 `futex_wait_bitset`）尚未将任务标记为 Blocked，由 `block_current_and_run_next` 负责标记。后者的调用场景中，等待队列（如 futex wait queue）已经完成了状态标记（`task_inner.task_status = TaskStatus::Blocked`），此函数只需执行 `take_current_task()` + `schedule()` 的实际切换。

---

=== 全局任务管理器（manager.rs）


`TaskManager`（`manager.rs`）是全局的任务管理结构，维护了两个核心映射表和一个就绪队列。`manager.rs` 还导出了两个跨模块使用的辅助函数。

*`ready_queue` 模块* —— 就绪任务队列，静态 `VecDeque<Weak<TaskControlBlock>>`：

- `add_task(task)` — 将任务加入队列尾部。内部先调用 `task_in_queue()` 遍历已有条目比对 `Arc::ptr_eq` 防止重复插入（重复插入会导致同一线程在就绪队列中出现多次，引发调度混乱）。若已存在，发出 warning 并跳过。
- `fetch_task()` — 从队列头部取出一个任务。循环跳过无效引用（`Weak::upgrade()` 返回 `None` 的条目自动丢弃），直到取到一个有效 TCB 或队列为空。这种懒清理策略避免了退出路径中显式遍历和清理就绪队列的需要，是 Low Coupling 原则的体现。
- `ready_procs_num()` — 返回就绪队列当前长度（含可能无效的 Weak 引用），用于 `/proc` 统计。

*`tid_to_task` 模块* —— TID → `Arc<TaskControlBlock>` 的 `BTreeMap` 映射（`BTreeMap` 而非 `HashMap` 保证了键的有序遍历，便于 `/proc` 信息按 TID 排序输出）：

- `insert(tid, task)` — 仅在 clone 成功后调用，将新 TCB 注册到全局映射表。
- `remove(tid)` — 仅在 IDLE 控制流中由 `switch()` 调用。移除不存在的 tid 会 panic，因为 IDLE 必须在 TCB 的最后一个强引用释放前完成移除。
- `tid2task(tid)` — 通过 TID 查找 TCB，用于信号定向发送（`send_signal_to_thread` 需要根据 tid 找到目标 TCB）。
- `get_all_tasks()` — 遍历整个映射表返回 `Vec<(tid, Arc<TCB>)>`，用于 `check_all_task_timers()` 和 `/proc` 信息收集。

*跨模块辅助函数*（定义于 `manager.rs`）：

- `wakeup_futex_task(task)` — futex 唤醒的通用入口。若任务已是 `Ready`（被信号提前唤醒），仅清零 `futex_key`/`futex_pa`；否则将状态设为 `Ready` 并加入就绪队列。这一防御性检查避免了信号和 futex_wake 竞态导致的重复入队。
- `check_all_task_timers()` — 遍历所有任务调用 `task.check_timer()`，在每次 `run_tasks()` 循环开始时调用（`processor.rs`），实现了 POSIX 间隔计时器的轮询触发。

*`pid_to_process`* 映射（`process/process.rs`）：`static PID_2_PROCESS_ARC: Lazy<Mutex<BTreeMap<usize, Arc<Process>>>>`——PID → `Arc<Process>` 的全局映射表。`Process::new()` 时插入，`Process::remove_from_global_map()` 时移除（在 wait4 回收 zombie 后调用）。使用 `Arc`（而非 `Weak`）因为 Process 的生命周期需要确保在 zombie 期间（等待父进程 wait）仍可被查询。

`TaskManager`/`ready_queue` 模块扮演了任务调度"全局控制器"的角色。它将所有子系统（clone、exit、futex、signal）对就绪队列的访问集中到 `add_task()` 和 `fetch_task()` 两个统一接口中。`ready_queue` 内部实现了防重复插入检查（遍历已有条目比对 `Arc::ptr_eq`）和无效引用自动清理（`fetch_task` 遇到 `Weak::upgrade()` 返回 `None` 时跳过并继续），使调用者无需感知队列的内部维护逻辑。

=== 处理器调度器


每个 Hart 拥有独立的 `Processor` 结构体，跟踪当前正在该核心上执行的任务。`Processor` 是每个硬件核心的"本地控制器"——它持有 `current: Option<Arc<TaskControlBlock>>` 记录当前运行的线程，以及 `idle_task_cx: Option<Box<TaskContext>>` 提供 idle 控制流的内核上下文。由于每个 Hart 只访问自己的 `Processor`，`PROCESSORS` 数组无需上锁。

核心调度函数 `schedule(task_cx_ptr)` 的工作流程如下：

1. 将当前任务的 `TaskContext` 指针保存到 `task_cx_ptr` 指向的位置；
2. 从就绪队列头部取出下一个任务；
3. 切换到该任务的页表（`memory_set.activate()`）；
4. 通过 `__switch` 汇编函数切换到新任务的上下文。

调度采用*不可抢占、协作式*的方式，上下文切换仅在以下时机发生：
- 时间片用尽（时钟中断触发 `suspend_current_and_run_next`）
- 任务主动阻塞（`block_current_and_run_next`、futex 等待、pipe 读写等待等）
- 任务退出（`exit_current_and_run_next`）

`Processor::run_tasks()` 是调度循环的主控制器函数。它在无限循环中依次执行 futex 超时检查 → 取当前任务 → 从就绪队列取下一任务 → 上下文切换，将调度决策逻辑集中在一个循环体中。特别地，`run_tasks()` 使用 `if let Some(cur_task) = take_current_task()` 和 `else` 分支区分"已有正在运行的任务"和"首次调度"两种场景，在单一函数中完成所有调度的控制流路由，避免了将调度策略分散到多个模块。

`__switch` 汇编函数是架构相关的上下文切换与架构无关的调度器之间的*间接层*。调度器只需传入两个 `*const TaskContext` 指针，不直接操作寄存器。`TaskContext` 结构体定义了最小化的 callee-saved 寄存器集（`ra`、`sp`、`s0-s11`），所有架构必须实现 `__switch` 操作此结构。这意味着移植到新 CPU 架构时，只需重写 `switch.S`，调度器的 Rust 代码完全不变——这正是 Protected Variations 原则的核心收益：将预期的不稳定点（CPU 架构差异）隔离在最小化的抽象接口之后。

=== 上下文切换（switch.rs）


上下文切换由 `switch.rs` 中的 Rust 包装函数和架构相关的汇编函数协同实现。该文件包含两个汇编外部声明的包装：

```rust
extern "C" {
    fn __switch(
        current_task_cx_ptr: *mut TaskContext,
        next_task_cx_ptr: *const TaskContext,
    ) -> usize;

    pub fn __abandon(tid: usize, next_task_cx_ptr: *const TaskContext) -> usize;
}

pub fn switch(current_task_cx_ptr: *mut TaskContext, next_task_cx_ptr: *const TaskContext) {
    let tid = unsafe { __switch(current_task_cx_ptr, next_task_cx_ptr) };
    if tid != 0 {
        tid_to_task::remove(tid);
    }
}
```

*`__switch` 汇编函数*：保存当前 CPU 的 callee-saved 寄存器（ra、sp、s0-s11）到 `current_task_cx_ptr` 指向的 `TaskContext`，从 `next_task_cx_ptr` 恢复寄存器。`__switch` 不返回给调用者——它返回到下一个任务的上下文中（即另一个曾经调用过 `__switch` 的线程继续执行）。返回值语义：
- 返回 `0`：正常的上下文切换，调用者是 `run_tasks()` 的 idle 控制流切向新任务
- 返回 `>0`（TID）：线程退出时通过 `abandon()` 调用 `__abandon`，该值用于在 `switch()` 包装中执行 `tid_to_task::remove(tid)`

*`__abandon` 汇编函数*：与 `__switch` 类似，但额外接受一个 `tid` 参数（通过 a0 寄存器传递）。它保存当前上下文到 `next_task_cx_ptr`（idle 的 TaskContext），然后返回 `tid`（透传给 `__switch` 的返回值）。用于退出路径中的"放弃当前任务"操作。

*Rust 包装 `switch()` vs 直接 `__abandon` 的区别*：
- `switch(current_cx, next_cx)` — 调度器在 `schedule()` 中使用。切换后若返回的 tid 不为 0，说明这是一个退出线程残留的上下文，执行 `tid_to_task::remove(tid)` 完成 TCB 从全局映射表的移除。注意：`tid_to_task::remove` 在此调用是因为 `__abandon` 在汇编中将 tid 透传为 `__switch` 的返回值。
- `abandon(tid)` — 在 `exit_current_and_run_next` 的末尾调用。直接调用 `__abandon(tid, idle_task_cx_ptr)`，不经过 `switch()` 的包装检查——因为 abandoning 不期望再次返回。实际上，`abandon` 的实现就是直接 call `__abandon`，且不会返回。

*`TaskContext` 结构体*：定义了上下文切换所需的最小寄存器集：

```rust
pub struct TaskContext {
    pub ra: usize,      // 返回地址（__switch 返回后跳转的目标）
    pub sp: usize,      // 栈指针
    pub s: [usize; 12], // s0-s11
}
```

首次创建任务时，`TaskContext::goto_trap_return(kernel_stack_top)` 将 `ra` 设为 `trap_return` 的入口地址、`sp` 设为内核栈顶。当调度器首次切换到该任务时，`__switch` 恢复这些寄存器后，"返回"到 `trap_return`，由 `trap_return` 从 Trap 上下文恢复用户态寄存器并通过 `sret` 回到用户态。

就绪队列存储 `Weak<TaskControlBlock>` 而非强引用。这意味着当线程退出并被标记为 Zombie 后，其 TCB 的最后一个强引用释放时，就绪队列中的 `Weak` 条目自动失效（`upgrade()` 返回 `None`），无需在退出路径中显式遍历和清理就绪队列。这种弱引用策略是调度器与任务生命周期之间实现低耦合的关键设计决策，避免了"退出必须通知调度器"的双向依赖。

== 线程创建：clone 系统调用


=== clone 机制


`clone` 系统调用是创建线程的核心接口，其原型为：

```
clone(flags, child_stack, parent_tid, child_tid, tls) -> tid
```

其中 `CloneFlags` 控制父子之间的资源共享程度：

#table(
  columns: 2,
  table.header([*标志*], [*含义*]),
  [`CLONE_VM`], [共享虚拟地址空间（线程创建）],
  [`CLONE_FILES`], [共享文件描述符表],
  [`CLONE_SIGHAND`], [共享信号处理表],
  [`CLONE_THREAD`], [标记为线程（同一线程组）],
  [`CLONE_VFORK`], [vfork 语义：父进程阻塞直到子进程 exec/exit],
  [`CLONE_CHILD_CLEARTID`], [子线程退出时清零 `child_tid` 地址],
  [`CLONE_SETTLS`], [设置线程局部存储（TLS）寄存器],
  [`CLONE_FS`], [共享文件系统信息（当前目录等）],
)


`CloneFlags` 使用 `bitflags!` 宏实现了资源策略的*组合式多态*。每个标志位对应一种"共享 vs. 复制"的策略选择，`clone_process()` 通过 `flags.contains(CloneFlags::CLONE_VM)` 等谓词进行策略分发，而非为每种 flag 组合编写独立的创建函数。当 Linux 内核新增 `CLONE_NEWUTS`、`CLONE_INTO_CGROUP` 等命名空间标志时，只需在 `CloneFlags` 中增加位定义并在 `validate_clone_flags()` 中添加合法性校验规则，现有的创建逻辑不受影响——这是 *Protected Variations* 原则在系统调用接口演化中的直接应用。

=== clone 的执行流程


1. 解析 clone flags，确定是否共享地址空间、文件描述符、信号表等；
2. 如果不共享（`!CLONE_VM`），则通过 `MemorySet::from_existed_user()` 创建父进程地址空间的 COW 副本；
3. 分配新的 TCB，分配内核栈：
   - `CLONE_VM` 时，子线程的用户栈由 `child_stack` 参数指定；
   - 非 `CLONE_VM` 时，创建独立的用户栈和 TrapContext 页；
4. 设置子线程的 TrapContext：将 `sepc` 设为父进程的 `sepc`（返回后从同一位置继续执行），`sp` 设为指定的栈指针；
5. 对于 `vfork`：将父进程标记为 `VforkBlocked`，记录等待的子线程 tid；
6. 将子线程加入就绪队列，返回子线程 tid。

`TaskControlBlock::clone_process()` 是本内核进程管理的核心创建逻辑，它采用独创的*两阶段创建模式*来规避锁顺序死锁：
在持有 `self.inner` 和 `self.process.inner` 的锁期间，仅提取构造子进程所需的数据——对共享资源执行 `Arc::clone()`（零拷贝引用计数递增），对 Copy 字段直接复制值。关键原则是*只读不写，不碰子进程的锁*；
释放所有父进程锁后，构造子进程 TCB，注册到进程任务列表，配置 TrapContext，处理 `CLONE_CHILD_SETTID`/`CLONE_VFORK` 等需要访问子进程或父进程的后置操作。此时可安全获取任意顺序的锁，因为不再存在"持有父锁 → 拿起子锁"的嵌套。
这种两阶段模式避免了在持有父进程锁的情况下访问子进程的 `process.meta_lock()` 或 `inner`，消除了经典 fork 实现中因反向锁顺序导致的潜在死锁。从 GRASP 视角看，*Creator 的职责被明确赋予 `clone_process()`*，因为它拥有完成创建所需的全部上下文（flags、stack、tls），并且能够正确管理创建过程中的锁顺序约束。

#figure(sequence((( [用户态], [clone flags、栈、TLS 等参数], [sys_clone] ), ( [sys_clone], [复制或共享 Process 资源，创建 TCB], [TaskManager] ), ( [TaskManager], [分配 tid 并加入 ready queue], [子任务] ))), caption: [clone 创建的关键交互。])


#figure(flow(([*解析并校验 clone flags*], [*按 flags 共享或复制 Process 资源*], [*创建 TaskControlBlock、trap context 与内核栈*], [*分配 tid，登记并放入 ready queue*])), caption: [clone 创建活动流程。])


=== clone3 系统调用


`clone3` 是 Linux 较新的 clone 变体（Linux 5.3+），使用结构体 `clone_args` 传参而非寄存器。与 `clone` 的直接参数传递不同，`clone3` 通过单一指针指向一个包含所有参数的结构体，支持更大的 flags（64-bit）和未来扩展字段。Ya2yOS 在 `os/src/syscall/task/clone3.rs` 中实现了 `clone3` 的参数解析（从用户态拷贝 `clone_args` 结构体，提取 flags、pidfd、child_tid、parent_tid、exit_signal、stack、tls 等字段），校验后直接复用 `clone` 的核心逻辑（`TCB::clone_process()` + `ready_queue::add_task()`）。这种实现方式遵循了 Protected Variations 原则：clone 语义的变化（新系统调用接口）被隔离在 syscall 入口的参数解析层，不渗透到核心创建逻辑。

== 程序执行：execve 系统调用


`execve` 系统调用用于加载并执行一个新的程序，其流程如下：

1. *参数拷贝*：从用户态拷贝路径名、argv、envp 到内核态字节数组；
2. *ELF 加载*：打开文件并解析 ELF 头部，验证魔数和架构兼容性。若文件并非 ELF 格式，则尝试解析 shebang（`#!`）行，转而加载解释器（如 `#!/bin/sh` → `/musl/busybox`）；
3. *新建地址空间*：
   - 创建全新的 `MemorySet`，初始化用户态地址空间布局；
   - 通过 `elf_loader` 将 ELF 各段（LOAD 段）映射到对应的虚拟地址，设置正确的页权限（R/W/X）；
   - 如有 INTERP 段，加载动态链接器；
4. *设置用户栈*：在用户栈顶构建 argv、envp、auxv（辅助向量）的数据结构，设置 `sp` 指向正确位置。辅助向量包括 `AT_PHDR`、`AT_PHENT`、`AT_PHNUM`、`AT_ENTRY`、`AT_PAGESZ` 等；
5. *替换地址空间*：将当前进程的 `memory_set` 替换为新构建的地址空间（旧空间在引用计数归零时被回收）；
6. *文件描述符处理*：关闭标记了 `O_CLOEXEC` 的文件描述符；
7. *更新 TrapContext*：设置 `sepc` 为 ELF 入口地址，`sp` 为新的用户栈顶。

`execve` 的实现是两种 GRASP 模式协同运作的范例。`Process::change_memory_set_and_sigtable()` 通过将 `ProcessInner.memory_set` 替换为新 `Arc<RwLock<MemorySet>>` 实现地址空间的原子切换——这是一个*间接层*操作，旧地址空间的回收完全由 `Arc` 的引用计数自动管理，exec 代码无需显式释放旧页面。同时，地址空间的创建通过 `MemorySetInner::from_elf()` 封装，底层 ELF 解析、段映射、页权限设置的复杂性被*保护*在这一抽象之后。若未来支持新的可执行格式（如脚本文件通过 shebang 间接加载解释器 ELF，该机制已在 `parse_shebang()` 中实现），只需扩展 `from_elf` 或提供替代的构造器，exec 的主流程不变。
`TCB::exec()` 在替换地址空间之前，必须在旧地址空间中完成 `CLONE_CHILD_CLEARTID` 的清理（向 `clear_child_tid` 写入 0 并调用 `futex_wake_up`），因为 exec 替换地址空间后该虚拟地址将不再有效。这一操作被放在 `exec()` 方法内部——它是唯一同时持有旧地址空间引用、`clear_child_tid` 值和 futex 唤醒知识的实体，自然成为这一清理职责的信息专家。

#figure(sequence((( [用户态], [传入路径、argv、envp], [sys_execve] ), ( [sys_execve], [加载 ELF 并建立新的 MemorySet], [文件系统 / 内存管理] ), ( [sys_execve], [替换进程映像并恢复用户态], [TaskControlBlock] ))), caption: [execve 执行的关键交互。])


#figure(flow(([*复制用户路径与参数*], [*打开并解析 ELF / 动态链接器*], [*构造新的地址空间、用户栈和 auxv*], [*原子替换进程映像并返回用户态*])), caption: [execve 活动流程。])


== 进程退出：exit 系统调用


总览exit过程进行工作:
#figure(sequence((( [当前线程], [sys_exit / exit_group], [TaskControlBlock] ), ( [TaskControlBlock], [释放线程资源，更新线程组退出状态], [Process] ), ( [Process], [唤醒父进程等待者并移交调度], [TaskManager] ))), caption: [exit 的关键交互。])


#figure(flow(([*记录退出码与终止信号*], [*清理线程资源；exit_group 同时终止兄弟线程*], [*形成 zombie / 可等待状态*], [*切换到下一可运行任务*])), caption: [exit 活动流程。])


=== exit 和 exit_group


- `exit`：终止当前线程（轻量级进程退出）。清理线程资源后将 TCB 状态设置为 Zombie；
- `exit_group`：终止当前线程组的所有线程。首先设置进程的 `SIGNAL_GROUP_EXIT` 标志和 `group_exit_code`，然后向所有兄弟线程发送 `SIGKILL` 信号，唤醒所有阻塞的兄弟线程。

=== exit 的资源清理


线程退出时，`exit_current_and_run_next` 执行以下清理工作：

1. *CLONE_CHILD_CLEARTID*：如果调用者通过 clone 设置了 `clear_child_tid`，则在退出时向该地址写入 0，并唤醒等待在该地址上的 futex；
2. *Robust List 处理*：遍历线程的 `robust_list`，对每个 robust futex 执行 `FUTEX_OWNER_DIED` 处理，设置 `FUTEX_WAITERS` 标志并唤醒等待者；
3. *vfork 唤醒*：如果父进程因 vfork 阻塞且等待当前线程，将父进程恢复为 Ready；
4. *线程栈回收*：回收线程的内核栈（`KernelStackOnHeap` Drop 时自动归还）和用户态线程栈（如果是 mmap 分配的）；
5. *TrapContext 页回收*：移除 TrapContext 的虚拟地址映射区域；
6. *兄弟线程唤醒*：将阻塞的兄弟线程唤醒（防止死锁）。唤醒时需从 futex 等待队列 `FUTEX_QUEUE_BITMAP` 中移除对应条目，防止后续 futex_wake 或超时处理根据已清理的 futex 字段错误地将本线程重新加入就绪队列；
7. *进程级回收*（当所有线程均已退出时）：
   - 回收进程中所有数据页
   - 清空文件描述符表
   - 清空文件系统信息
   - 设置退出码
   - *Reparent*：将孤儿子进程重新挂载到 initproc（pid=1）
   - 向父进程发送 `SIGCHLD` 信号，唤醒 `waitpid` 等待者
   - 移除 /proc 目录下的进程目录和文件
8. 将 TID 传递给 IDLE 控制流，由 IDLE 负责释放 TCB 的最后一层引用。

exit 的资源清理展示了多项降低耦合的设计决策。
（1）`Weak<TaskControlBlock>` 在就绪队列和 `ProcessMeta.tasks` 中的使用：线程标记为 Zombie 后，这些 `Weak` 引用自动失效，退出路径无需显式遍历和清理就绪队列，实现了退出逻辑与调度器之间的解耦。
（2）兄弟线程唤醒时将其从 `FUTEX_QUEUE_BITMAP` 中移除，避免了 futex 子系统与线程生命周期之间的隐式耦合——如果不清理，后续 futex_wake 可能基于已失效的 futex 字段唤醒一个已退出线程。
`AtomicWaker` 在退出通知中充当关键的间接层。当进程的最后一个线程退出时，`parent.meta_lock().child_exit_event.wake()` 唤醒在 `waitpid` 上阻塞的父进程。父进程通过在 `child_exit_event` 上注册 waker 来等待，而通知方（退出的子进程）无需知晓父进程的身份——它只需调用 `wake()`。这一间接层使得父子进程之间关于"退出通知"的依赖仅通过一个 `AtomicWaker` 字段关联，而非通过直接的函数回调或任务句柄。
进程级回收（第 7 步）中，`Process` 作为信息的集中持有者履行清理职责——它拥有 `children` 列表（用于 reparent）、`fd_table`（用于清空文件描述符）、`fs_info`（用于清空文件系统信息）、`sig_table`（用于读取/设置退出码）。将这些清理操作放在 `Process::exit_and_reparent()` 和 `Process::remove_from_global_map()` 中，遵循了"谁拥有数据，谁负责清理"的信息专家原则，避免了将清理逻辑分散在 `exit_current_and_run_next` 的长函数中。

=== futex 子系统（futex.rs）


`os/src/task/futex.rs` 实现了 futex（Fast Userspace muTEX）系统调用，是进程管理子系统中代码量最大的单文件（~600 行）。futex 是 Linux 用户态同步原语（`pthread_mutex`、`pthread_cond`、`semaphore` 等）的内核支撑——用户态通过原子操作在无竞争时避免系统调用开销，仅在需要阻塞等待或唤醒时才进入内核。

*futex 等待队列架构*：

```rust
pub static FUTEX_QUEUE_BITMAP: Lazy<Mutex<BTreeMap<usize, BitsetWaitQueue>>> = ...;

pub struct FutexWaiter {
    pub task: Weak<TaskControlBlock>,
    pub bitset: u32,
    pub futex_key: usize,  // 全局唯一键，与 TCBInner.futex_key 对应
}
```

每个等待者通过 `(futex_pa, futex_key)` 二元组标识。`futex_key` 由全局原子计数器 `FUTEX_KEY_COUNTER` 递增分配（从 1 开始，0 表示"已清理"）。这种设计解决了经典问题：一个线程可能先后在同一个 futex 字上等待多次，用物理地址无法区分不同的等待实例——`futex_key` 提供了实例级别的唯一性。

*核心操作流程*：

`sys_futex(uaddr, futex_op, val, timeout, uaddr2, val3)` 是 futex 系统调用的入口：

1. 解析 `futex_op` 低 7 位得到 `FutexCmd`（Wait/Wake/Requeue/WaitBitset/WakeBitset）
2. 解析高位的 `FutexOpt` bitflags（FUTEX_PRIVATE_FLAG / FUTEX_CLOCK_REALTIME）
3. 校验 `uaddr` 4 字节对齐
4. 用户态 VA → PA 转换：使用 `translate_user_va_safe()` 而非直接查页表。该函数先通过 `copy_from_user` 触发懒分配（确保页面已映射），再查询页表得到 PA。避免了懒分配页面上直接查页表导致的 panic
5. 对于 Wait 操作，先读取 futex 字的当前值进行原子性检查（若 `current_val != val` 则返回 `EAGAIN`，因为值已被其他线程修改，用户态应重试）；同时检查 `FUTEX_OWNER_DIED` 标志进行 robust futex 恢复

*futex_wait_bitset 的唤醒后检查顺序*（内核对唤醒原因的分类）：

1. 信号唤醒 → 返回 `EINTR`：检查 `sig_pending & !sig_mask` 是否非空，若是则从 `FUTEX_QUEUE_BITMAP` 移除自己的等待条目并返回错误
2. 超时唤醒 → 返回 `ETIMEDOUT`：检查 `futex_timedout` 标志。定时器超时处理器 `handle_timer()` 已将等待者从队列移除并设置了此标记
3. 正常唤醒 → 返回 `Ok(0)`：被 `futex_wake_up_bitset` 通过 `wakeup_futex_task` 唤醒

*futex_wake_up_bitset 的 bitset 匹配逻辑*：遍历等待队列，对每个等待者检查 `bitset & waiter.bitset != 0`。匹配的唤醒并移除，不匹配的放回队列尾部（保持顺序）。`FUTEX_WAIT_BITSET` 操作允许线程指定一个 bitset，唤醒者可以精确选择唤醒哪些线程——这是 `pthread_cond_broadcast` vs `pthread_cond_signal` 的内核基础。

*futex_requeue*：将等待者从一个 futex 重新排队到另一个 futex（用于 `pthread_cond_wait` 的实现：等待在 cond futex 上的线程被唤醒后，重新排队到 mutex futex）。最多唤醒 `max_wakeup` 个，最多迁移 `max_requeue` 个。

*futex_queue_key 的 PRIVATE 标志优化*：若 `FUTEX_PRIVATE_FLAG` 被设置（意味着 futex 不会被不同进程共享），则使用 `memory_token ^ uaddr` 作为队列键（而非物理地址 `pa`）。这是因为同一进程内的不同虚拟地址不可能映射到同一物理地址——使用 token^uaddr 避免了 COW 后的物理地址冲突（fork 后父子进程的同一 VA 可能映射到不同 PA）。

*Robust Futex*：`handle_futex_when_exit()` 和 `handle_futex_death_entry()` 实现了 robust futex 的死者恢复机制，见 2.5.2 节步骤 2 的描述。实现细节：
- `handle_futex_death_entry` 用"穷人的 CAS"重试循环（读取 → 检查 ownership → 写入 `FUTEX_OWNER_DIED` → 回读验证）处理稳健 futex 的所有权转移，因为 RISC-V 没有硬件 CAS 指令（LR/SC 在跨页或特定场景下受限）
- 遍历上限 `ROBUST_LIST_LIMIT = 2048` 防止损坏的链表导致无限循环
- `futex_owner_alive_in_current_process()` 在恢复前检查记录的 owner TID 是否仍在同一进程存活，避免错误地将活线程持有的 futex 标记为死

futex 队列的 key 设计（PA vs token^uaddr）通过 `futex_queue_key()` 函数封装了"是否跨进程共享"的变化点。`FUTEX_PRIVATE_FLAG` 的引入不影响上层 `futex_wait_bitset`/`futex_wake_up_bitset` 的逻辑——它们只看到统一的 `queue_key`。

---

== 进程等待：wait4 系统调用


`wait4` 系统调用允许父进程等待子进程的状态变化：

1. 遍历当前进程的子进程列表；
2. 查找是否有子进程处于 Zombie 状态或状态发生了变化（`WUNTRACED`/`WCONTINUED`）；
3. 如果找到符合条件的子进程：
   - 将退出状态写入 `status` 参数（使用 Linux 兼容的宏编码）；
   - 清理该子进程的 TCB（通过 `TidHandle` 的 Drop 回收资源）；
   - 返回子进程的 pid；
4. 如果没有找到：
   - `WNOHANG` 标志：立即返回 0；
   - 否则：在 `child_exit_event` 上阻塞等待，直到子进程状态发生变化。

`waitpid` 和 `waitid` 共用同一个子进程扫描流水线，但通过 `WaitPid` 枚举实现 PID 选择策略的多态分发：`WaitPid::Any`（等待任意子进程）、`WaitPid::Pid(usize)`（等待特定 PID）、`WaitPid::Pgid(u32)`（等待同一进程组的子进程）。`apply()` 方法为每种变体提供了统一的过滤接口，使得 `waitpid` 的 `pid` 参数（支持 `>0`、`0`、`-1`、`<-1` 四种语义）和 `waitid` 的 `idtype` 参数（`P_ALL`、`P_PID`、`P_PGID`）通过各自的入口函数映射为 `WaitPid` 枚举后，复用完全相同的子进程扫描逻辑。此外，`WaitOption` bitflags 通过 `WUNTRACED`/`WSTOPPED`/`WCONTINUED`/`WEXITED` 控制事件类型过滤，进一步扩展了策略组合空间。
`ProcessMeta` 是子进程等待的信息专家——它持有 `children: Vec<Weak<Process>>`（子进程列表）、`child_exit_event: AtomicWaker`（等待通知通道）、`stopped_signal`/`continued_signal`（子进程状态变化事件）、`exit_signal`（用于 `__WALL`/`__WCLONE` 过滤）——所有这些数据都属于"父子进程间状态协调"这一关注领域，自然应由 `ProcessMeta` 统一管理。

#figure(sequence((( [父进程], [wait4 / waitid 请求], [sys_wait] ), ( [sys_wait], [检查子进程状态，必要时阻塞], [Process] ), ( [已退出子进程], [返回 pid 与 wait status，回收资源], [父进程] ))), caption: [wait4 的关键交互。])


#figure(flow(([*按 pid / pgid 匹配子进程*], [*若有可报告状态则生成 wait status*], [*若仍有子进程则阻塞等待唤醒*], [*回收 zombie 并向用户返回结果*])), caption: [wait4 活动流程。])


== PID/TID 分配、内核栈与系统信息


=== ID 分配器与 TidHandle（tid.rs / utils/id_allocator.rs）


`os/src/task/tid.rs` 负责 TID 的生命周期管理，依赖 `os/src/utils/id_allocator.rs` 提供的通用 ID 分配器。

*`IdAllocator`（`utils/id_allocator.rs`）*：

```rust
pub struct IdAllocator {
    next_id: usize,         // 递增计数器
    recycled: Vec<usize>,   // 回收栈（LIFO 复用）
}
```

- `alloc()` — 优先从 `recycled` 栈弹出复用 ID（LIFO 策略，减少 ID 碰撞的可能性），若回收栈为空则从 `next_id` 递增。使用 `checked_add(1)` 防溢出返回 `None`
- `dealloc(id)` — 将 ID 推入回收栈，供后续分配复用

全局实例：`GLOBAL_ID_ALLOCATOR: Mutex<IdAllocator>`，起始 ID 为 1（保留 0 为 IDLE）。

*`TidHandle`*：

- `alloc()` — 从全局分配器获取新 TID，失败返回 `None`（不 panic，由调用者处理资源不足）
- `Deref<Target = usize>` — 透明解引用，使 `TidHandle` 可以在任何需要 `usize` 的场景中直接使用
- `Drop` — zombie 状态保留 PID 不立即回收，防止 TID 在僵尸进程尚未被 wait 的情况下被复用。实际回收时机由 IDLE 控制流在 `abandon` 后显式控制

`IdAllocator` 和 `TidHandle` 是本设计中两个核心的纯虚构类，它们并不直接映射到任何操作系统领域概念（如"进程"或"文件"），而是为了解决横切关注点而独立设计的辅助抽象。
*`IdAllocator`* 将"分配唯一标识符"这一通用需求从具体的 TID/PID 管理代码中抽象出来，成为一个可复用的纯虚构组件。它通过 `checked_add(1)` 防止溢出返回 `None`，避免 panic。这一纯虚构使得 TID 分配从 TrustOS 原型的专用实现进化为可复用的通用库——文件描述符（FD）分配器可直接复用同一实现，无需重复编写"递增 + 回收"逻辑。
*`TidHandle`* 是一个更富设计深度的纯虚构。它将一个原始 `usize` 的 TID 提升为具有生命周期管理能力的 RAII 对象——通过 `Deref<Target = usize>` 保持与原始整数的透明使用体验，同时通过 `Drop` 控制回收时机。在本实现中，`TidHandle::drop()` 延迟回收 TID（zombie 状态保留 PID），使得 TID 分配器无需感知内核调度状态。

=== 内核栈管理（kernel_stack.rs）


`os/src/task/kernel_stack.rs` 定义了 `KernelStackOnHeap`，封装了内核栈的分配与地址计算：

```rust
pub struct KernelStackOnHeap {
    pages: ContinuousPages,
}

impl KernelStackOnHeap {
    pub fn new() -> Self {
        Self { pages: ContinuousPages::new(4).expect("fail to alloc KStack!") }
    }
    pub fn base(&self) -> usize { self.pages.base() }
    pub fn top(&self) -> usize { self.pages.base() + 4 * PAGE_SIZE }
}
```

每个 TCB 创建时分配 4 个连续物理页（16 KiB，假设 PAGE_SIZE=4K）作为内核栈。`top()` 返回栈顶地址（高地址），内核栈向下增长。`TaskContext::goto_trap_return(kernel_stack_top)` 将初始 `sp` 设为栈顶，确保首次调度时内核栈从高端开始使用。

`KernelStackOnHeap` 实现了 `Drop`（通过 `ContinuousPages` 的 Drop），在 TCB 释放时自动归还物理页。与 `TidHandle` 的设计呼应——两者都是 RAII 资源句柄，使得 TCB 的 Drop 不需要显式的资源释放代码。

=== 辅助向量（aux.rs）


`os/src/task/aux.rs` 定义了 ELF 辅助向量（auxiliary vector）的类型系统，用于 `execve` 时向新程序传递内核信息：

```rust
pub enum AuxType {
    NULL = 0, PHDR = 3, PHENT = 4, PHNUM = 5, PAGESZ = 6,
    ENTRY = 9, UID = 11, EUID = 12, GID = 13, EGID = 14,
    PLATFORM = 15, HWCAP = 16, CLKTCK = 17, RANDOM = 25,
    EXECFN = 31, SYSINFO_EHDR = 33, MINSIGSTKSZ = 51, ...
}

pub struct Aux {
    pub aux_type: AuxType,
    pub value: usize,
}
```

`execve` 在构建用户栈时，从低地址到高地址依次压入 `auxv` 数组的每一项（`{type, value}` pair），再压入 `envp[]` 指针数组、`argv[]` 指针数组、`argc`。动态链接器（`ld.so`）从栈上读取 `auxv` 获取 `AT_PHDR`（程序头表地址）、`AT_ENTRY`（入口地址）、`AT_PAGESZ`（页面大小）、`AT_RANDOM`（16 字节随机数，用于 stack canary 和 ASLR 种子）等关键信息。

=== 系统信息快照（sysinfo.rs）


`os/src/task/sysinfo.rs` 定义了 `sysinfo(2)` 系统调用使用的 `Sysinfo` 结构体：

```rust
pub struct Sysinfo {
    pub uptime: usize,      // 开机秒数
    pub loads: [usize; 3],  // 1/5/15 分钟负载
    pub totalram: usize,    // 总内存
    pub freeram: usize,     // 可用内存 (totalram - 内核镜像大小)
    pub sharedram: usize,   // 共享内存
    pub bufferram: usize,   // 缓冲内存
    pub totalswap: usize,   // 总交换空间
    pub freeswap: usize,    // 可用交换空间
    pub procs: u16,         // 当前进程数
    pub mem_unit: u32,      // 内存单位（字节）
    ...
}
```

`Sysinfo::new(uptime, totalram, procs)` 构造函数通过 `ekernel` 符号（内核镜像结束地址）计算 `freeram = totalram - ekernel`。这是 Linux `sysinfo` 的简化实现，`loads` 当前固定为 `[0; 3]`，`sharedram`/`bufferram`/`totalswap`/`freeswap` 暂为 0。

== 初始进程（initproc）


`INITPROC` 是内核启动后创建的第一个用户态进程（pid=1），定义于 `os/src/task/mod.rs`：

```rust
pub static INITPROC: Lazy<Arc<TaskControlBlock>> = Lazy::new(|| {
    let initproc = open("/initproc", OpenFlags::O_RDONLY, NONE_MODE)
        .expect("open initproc error!")
        .file()
        .expect("initproc can not be abs file!");
    let elf_data = initproc.inode.read_all().unwrap();
    let res = TaskControlBlock::new(&elf_data);
    res
});

pub fn add_initproc() {
    ready_queue::add_task(&INITPROC);
    tid_to_task::insert(INITPROC.tid(), &INITPROC);
}
```

初始化流程：

1. 通过 VFS 打开根文件系统中的 `/initproc` 可执行文件；
2. 读取完整的 ELF 数据到 `elf_data` 字节数组；
3. 调用 `TaskControlBlock::new(&elf_data)` 创建初始进程的 TCB：内部完成 ELF 加载（`MemorySetInner::from_elf`）、TID 分配（pid=1）、内核栈分配（`KernelStackOnHeap::new()`）、Process 构造（含 `FdTable::new_with_stdio()` 和 `FSInfo::new_initproc()`）、初始 TrapContext 设置（入口地址 + 用户栈顶）；
4. `add_initproc()` 在 `main.rs` 中最后调用，将 INITPROC 加入就绪队列和全局 `tid_to_task` 映射表，此后 `run_tasks()` 首次调度即可切到 `initproc` 执行。

`Lazy` 保证了 INITPROC 在首次访问时才初始化（而非在静态初始化阶段），避免了对尚未就绪的 VFS 和物理页分配器的提前依赖——这是一个延迟初始化模式在内核启动中的关键应用。

`initproc` 承担以下特殊职责：
- 接收被 reparent 的孤儿进程（`exit_and_reparent`）：当某个进程退出但未 wait 其子进程时，这些孤儿被重新挂载到 pid=1 的 initproc
- 回收所有孤儿子进程的退出状态（防止僵尸进程堆积）：initproc 在用户态循环调用 `waitpid(-1, ...)` 回收孤儿
- 作为用户态服务管理器启动其他系统服务（如 user_shell）

`TaskControlBlock::new(&elf_data)` 是 initproc 的唯一创建者。它执行从原始 ELF 字节到可调度 TCB 的完整引导流程——加载 ELF 创建 `MemorySet`、分配 TID 和内核栈、构造 `Process`（连同 `FdTable::new_with_stdio()` 和 `FSInfo::new_initproc()`）、设置初始 `TrapContext`（`TaskContext::goto_trap_return()`）——所有这些创建决策集中在一个函数中，因为它是唯一同时拥有 ELF 数据、PID=1 的身份和 initproc 特殊资源需求（stdio、根目录" / "）的知识的实体。

initproc 作为孤儿进程的最终回收者，是进程关系清理的信息专家。`Process::exit_and_reparent()` 首先收集当前进程的所有孤儿 `children`，然后获取 pid=1 的 `initproc`，将孤儿的 `parent_pid` 修改为 1，并将孤儿链接到 initproc 的 `children` 列表中，最后向 initproc 发送 `SIGCHLD` 唤醒其 waitpid 循环。此逻辑被放在 `Process::exit_and_reparent()` 中而非分散在 exit 路径中，遵循了信息专家原则——`Process` 拥有 children 列表和修改 parent_pid 的权限，自然应承担孤儿转移的职责。

#figure(relation(([*前置条件*\参数、用户地址和 flags 已校验], [*操作*\任务/进程状态与资源变更], [*后置条件*\errno 或 Linux ABI 可观察结果])), caption: [任务管理操作契约。])


== 调度策略与未来展望


当前 Ya2yOS 采用简单的 *FIFO 协作式调度*，不区分优先级。在时钟中断到来时，当前任务被放回就绪队列末尾，实现轮转效果。未来可改进的方向包括：

1. *CFS（完全公平调度器）*：基于 vruntime 的公平调度，更好地支持交互式和批处理混合负载；
2. *实时调度类*：SCHED_FIFO / SCHED_RR 策略，为实时任务提供确定性延迟；
3. *多核负载均衡*：当前多核支持已具备基础（Hart 各自独立调度），但缺乏主动的负载均衡机制；
4. *NUMA 感知调度*：物理内存分配和任务调度的 NUMA 亲和性优化。

当前调度架构已为上述未来改进预留了受保护的变化点。`ready_queue` 模块对外只暴露 `add_task()` 和 `fetch_task()` 两个接口，调度策略（FIFO）被完全封装在模块内部。未来替换为 CFS 红黑树或实时调度优先级队列时，只需修改 `ready_queue` 的内部实现（将 `VecDeque` 替换为 `BTreeMap<vruntime, ...>` 或多级优先级队列），`run_tasks()`、`suspend_current_and_run_next()`、`block_current_and_run_next()` 等所有调用方无需任何修改。此外，`TaskControlBlockInner` 已预留 `nice: i32` 字段（范围 -20..19，默认 0），为 CFS 的权重计算提供了必要的数据基础。
