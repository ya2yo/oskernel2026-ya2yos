#import "../diagrams.typ": flow, relation, sequence

= 进程、线程与程序映像

本章对应 `os/src/task/`、`os/src/syscall/task/` 与
`os/src/mm/memory_set/elf_loader.rs` 在当前工作树中的实现。它说明已经接入的
Linux 风格任务模型、创建和回收路径；调度策略、资源回收和部分 `clone3` 字段仍有
明确限制，文中据实标出而不将其表述为规划中的完整能力。

== 模块边界与对象模型

#table(
  columns: (1.45fr, 3fr),
  table.header([*位置*], [*当前职责*]),
  [`os/src/task/process/process.rs`], [`Process`、全局 PID 映射、父子关系、进程组/会话和线程组退出元数据],
  [`os/src/task/task/task.rs`], [`TaskControlBlock`、用户 trap 上下文、内核栈，以及 `new`、`clone_process`、`exec`],
  [`os/src/task/manager.rs`], [TID 到 TCB 的映射、futex 唤醒和阻塞任务计时器补扫],
  [`os/src/task/scheduler/`], [编译期选择的 CFS/RR 策略、就绪队列门面与运行时间记账],
  [`os/src/task/processor.rs`], [按 Hart 保存当前任务和 idle 上下文，执行调度循环],
  [`os/src/task/mod.rs`], [suspend、block、stop、exit 和 initproc 的控制流入口],
  [`os/src/syscall/task/`], [`clone`、`clone3`、`execve`、`exit`、`waitpid` 与 `waitid` 的 ABI 入口],
)

Ya2yOS 将可调度执行单元与进程级资源分开建模。`TaskControlBlock`（TCB）代表
一个线程，持有独立的 `TidHandle`、`KernelStackOnHeap`、用户 trap 上下文、
内核切换上下文和线程私有的信号、futex、凭据、计时器等状态。`Process` 代表
线程组和其进程级资源，持有当前地址空间、信号动作表、文件描述符表、文件系统
上下文及 PID。TCB 通过 `Arc<Process>` 归属到一个 `Process`；同一线程组内的
多个 TCB 因而观察同一组进程资源。

```rust
// 代码块只摘录影响本章流程的字段；源码还包含调度、资源限制等字段。
pub struct Process {
    memory_set: ResourceSlot<MemorySet>,
    sig_table: ResourceSlot<Mutex<SigTable>>,
    pub fd_table: Arc<FdTable>,
    pub fs_info: Arc<FSInfo>,
    pub pid: usize,
    home_hart: usize,
    pub meta: Mutex<ProcessMeta>,
    pub rlimit_fsize: Mutex<RLimit>,
}

pub struct TaskControlBlock {
    tid: TidHandle,
    kernel_stack: KernelStackOnHeap,
    pub process: Arc<Process>,
    cpu_affinity: AtomicUsize,
    scheduled_hart: AtomicUsize,
    on_cpu: AtomicBool,
    pub interrupted: AtomicBool,
    pub interrupt_waker: AtomicWaker,
    pub(crate) sched_entity: SchedEntity,
    inner: RemoteTlbMutex<TaskControlBlockInner>,
}
```

`ResourceSlot<MemorySet>` 和 `ResourceSlot<Mutex<SigTable>>` 的作用是保护
`Arc` 指针整体的读取和替换。地址空间本身由 `MemorySet` 内部同步；`FdTable`
和 `FSInfo` 也各自管理内部并发。因此，`execve` 替换映像时可调用
`Process::change_memory_set_and_sigtable()` 更换地址空间和信号动作表，而普通
文件操作不需要持有一个覆盖整个 `Process` 的大锁。

`ProcessMeta` 保存不属于资源槽位的线程组关系和可等待状态：活线程弱引用列表、
子进程弱引用列表、父 PID、`is_child_subreaper`、进程组 ID（PGID）、会话 ID（SID）、
子进程事件 `AtomicWaker`、退出信号、线程组退出码、stop/continue 事件、终止信号、
资源使用快照、`comm` 和 `personality`。全局 `PID_2_PROCESS_ARC` 是
`BTreeMap<usize, Arc<Process>>`；进程创建时插入，父进程实际回收 zombie 时才由
`Process::remove_from_global_map()` 删除。`tasks` 与 `children` 都是弱引用列表，
退出或等待路径会清理其中已经失效的引用。

#figure(
  relation((
    ([*Process*\地址空间、fd、fs、信号动作、PID], [*TCB*\TID、内核栈、trap/task context、线程私有状态], [*ProcessMeta*\父子关系、PGID/SID、退出与等待事件]),
    ([*Scheduler*\feature-selected ready queue], [*TaskManager*\TID 映射、timer/futex 唤醒], [*Processor*\每 Hart 当前任务、idle context])
  )),
  caption: [当前任务管理对象关系。]
)

=== 任务状态与生命周期

`TaskStatus` 当前定义六种状态：

#table(
  columns: (1.25fr, 3fr),
  table.header([*状态*], [*实现含义*]),
  [`Ready`], [已可运行，可进入当前编译策略的就绪队列],
  [`Running`], [被当前 Hart 的 `Processor.current` 持有并正在执行],
  [`Blocked`], [等待 futex、I/O、异步事件等；唤醒路径将其改回 `Ready`],
  [`Stopped`], [被 stop 类信号停止，等待 `SIGCONT` 等恢复路径],
  [`VforkBlocked`], [父线程因 `CLONE_VFORK` 等待指定子线程 `exec` 或退出],
  [`Zombie`], [当前线程已退出；进程是否可由父进程回收取决于全部线程是否退出],
)

TCB 的 trap 上下文实际位于用户地址空间的专用映射页，`TaskControlBlockInner`
保存其物理页号和起始虚拟地址；`TaskContext` 保存内核态切换所需的 callee-saved
寄存器。新任务的 `TaskContext::goto_trap_return()` 将返回地址设为
`trap_return`，首次被调度后从 trap 上下文恢复用户寄存器并回到用户态。

`TidHandle` 使用一个从 1 起始的全局 `IdAllocator`，initproc 因而获得 PID/TID 1。
当前 `TidHandle::Drop` *不*归还 ID：注释明确指出共享的 PID/TID 分配器若在 zombie
被 `wait` 前重用 ID，会覆盖全局 PID 映射。因此本快照中 TID/PID 单调消耗，不应把
它描述为已实现的 ID 回收机制。

== 调度策略与全局任务表

`task::ready_queue` 是稳定门面，具体实现由互斥的 `scheduler-cfs` 与 `scheduler-rr`
Cargo feature 在编译期选择。默认 `scheduler-cfs` 使用所有 Hart 共享的
`Mutex<CfsRunQueue>`，内部 `BinaryHeap<CfsEntry>` 以 `(vruntime, tid)` 反向排序，使
`pop()` 取得虚拟运行时间最小的任务。`fetch_task(hartid)` 会保留暂时不满足当前 Hart
CPU affinity 的堆项，并从同一共享堆中选择可运行任务；因此不存在每 Hart 独立的
CFS heap 或 `min_vruntime`。TCB 的 `SchedEntity` 保存 `vruntime`、`exec_start` 与
原子 `on_rq` 去重位；任务离开 `Processor.current` 时按硬件 tick 统计执行时间，并
使用 Linux nice -20..19 的权重表折算 `vruntime`。共享队列的 `min_vruntime` 单调推进，
新任务首次入队时被放置到这一全局坐标。

`scheduler-rr` 保留原有 FIFO 语义：单个全局 `VecDeque` 从队尾入队、队首出队，
辅以 TID 集合去重，并在取任务时跳过不属于当前 `scheduled_hart` 的项。CFS 与 RR
都通过同一 `add_task()` / `fetch_task()` API 服务唤醒路径。`tid_to_task` 独立维护
`BTreeMap<usize, Arc<TaskControlBlock>>`，用于按 TID 查找线程、遍历计时器候选和
在线程退出的 idle 控制流中移除条目。

每个 Hart 有一个 `Processor`，其中包含 `current: Option<Arc<TaskControlBlock>>` 和
`idle_task_cx`。`run_tasks()` 检查普通计时器、阻塞任务计时器和 futex 超时，记账并
取出当前任务；仅将 `Ready`/`Running` 实体重新入队，再由所选策略统一选择下一个任务
并通过 `switch()` 进入其 `TaskContext`。`Blocked`、`VforkBlocked` 与 `Stopped` 留在
队列外，直到相应唤醒路径把它们恢复为 `Ready`。

时钟中断仍以 100Hz 调用 `suspend_current_and_run_next()` 驱动抢占，自愿 yield 和阻塞
路径也可切回调度循环。因此这是基于 nice 加权 `vruntime` 的简化 CFS，而非 Linux
完整调度子系统：尚无 target latency/sched period、调度组、完整的跨 Hart 迁移、work
stealing 或负载均衡策略，也没有实现实时调度类。已有远程入队和空闲 Hart 通知，用于
唤醒协作而非完整负载均衡。feature 只选择内核内部 runqueue；`sched_setscheduler(2)`
等用户 ABI 仍是兼容 stub，不提供运行时 CFS/RR 切换。

#figure(
  sequence(((
    [用户态 / 时钟中断], [trap 后调用 suspend、block、stop 或 exit], [任务控制流]),
    ([任务控制流], [状态更新，必要时回到 scheduler 或等待队列], [Scheduler]),
    ([Scheduler], [按 CFS/RR 策略取下一个 `Ready` 任务], [Processor]),
    ([Processor], [`switch` 到任务 `TaskContext`], [任务控制流])
  )),
  caption: [调度入口与状态转换的当前控制流。]
)

== rseq：线程级可重启序列

Ya2yOS 在 `os/src/task/rseq.rs` 实现 classic 32 字节 Linux `rseq(2)` ABI。注册状态
保存在 TCB 的 `TaskControlBlockInner.rseq` 中，因此它属于线程而不是 `Process`；
`abi_addr` 必须按 32 字节对齐，长度必须为 32，内核会先探测并初始化用户 ABI 区域。
当前实现发布当前 Hart 的 CPU 编号；由于尚无 NUMA 拓扑或 per-mm 并发 ID，`node_id`
和 `mm_cid` 写为 0，未通过 auxv 宣布更新的扩展布局。

注册时只接受 `flags == 0`；重复注册按 ABI 参数返回 `EBUSY` 或参数错误。注销使用
`RSEQ_FLAG_UNREGISTER`，要求地址、长度和签名与当前注册完全匹配，否则返回
`EINVAL` 或 `EPERM`。线程执行 `execve` 时清除旧映像中的 rseq 注册，`CLONE_VM` 创建的
子线程也清除 rseq，以避免继承指向旧线程局部区域的用户指针；普通 fork 路径则继承
该状态。

在返回用户态前，内核将当前 Hart 写入 `cpu_id_start/cpu_id`，检查用户的
`rseq_cs` 描述符和 abort 签名：若指令指针仍位于可重启临界区内，就清除 `rseq_cs`
并把 trap 返回地址改为 `abort_ip`；否则仅清除已完成的描述符。用户内存或描述符
无效时停止继续尝试，并按错误路径向线程交付 `SIGSEGV`。这使 rseq 的 CPU 发布和
临界区回滚与 TCB 调度、exec、clone 生命周期保持一致。

== 创建：clone 与 clone3

`sys_clone()` 先解析低位退出信号和 `CloneFlags`，校验不支持或互斥的组合，再调用
`TaskControlBlock::clone_process()`；成功后将新 TCB 加入 `ready_queue` 并返回其
TID。`clone_process()` 先分配 TID 和四页内核栈，随后采用两个阶段：第一阶段读取
父 TCB/Process 的状态并决定共享或复制策略，第二阶段在不持有父任务锁时构造子
TCB、建立 trap 上下文、登记进程任务表和全局 TID 表。这样避免了创建过程中嵌套持有
父子任务锁。

#table(
  columns: (1.6fr, 3fr),
  table.header([*标志或情形*], [*当前实现*]),
  [`CLONE_VM`], [共享父 `MemorySet`，但不必然共享 `Process`：与 `CLONE_THREAD` 一起使用时复用父线程组；非线程型 `CLONE_VM` 子进程创建新的 `Process`/PID，但引用同一地址空间。非 `CLONE_VM` 路径复制用户地址空间。`CLONE_VM` 子任务通常清空 alternate signal stack 并清除 rseq；`CLONE_VM | CLONE_VFORK` 是例外。],
  [`CLONE_FS`], [共享 `FSInfo`；否则以 `FSInfo::from_another()` 复制。],
  [`CLONE_FILES`], [共享 `FdTable`；否则以 `FdTable::from_another()` 复制。],
  [`CLONE_SIGHAND`], [共享信号动作表；否则复制，`CLONE_CLEAR_SIGHAND` 则创建空表。],
  [`CLONE_THREAD`], [复用父 `Process`、PID、父 PID 与计时器；非线程创建新的 `Process`，其 PID 为新 TID，并登记为父进程子项。],
  [`CLONE_SETTLS`], [把 `tls` 写入子 trap 上下文的线程指针寄存器。],
  [`CLONE_PARENT_SETTID` / `CLONE_CHILD_SETTID`], [分别向父/子地址空间的用户指针写入新 TID。],
  [`CLONE_CHILD_CLEARTID`], [记录用户指针，线程退出或 `exec` 替换旧地址空间前写零并 futex 唤醒。],
  [`CLONE_VFORK`], [父 TCB 记录子 TID 并转为 `VforkBlocked`；子线程 `exec` 或退出时唤醒相应父线程。],
)

子 trap 上下文从父线程复制，子路径把返回寄存器 `a0` 设为 0；用户指定非零栈时再
覆盖 `sp`。非 `CLONE_THREAD` 的普通子进程会继承父 PGID/SID、设置退出信号，并创建
`/proc/<pid>` 目录项。`CLONE_THREAD` 不创建新的 `/proc` 进程目录。

`sys_clone3()` 已接入 `clone_args` 的用户内存读取和版本长度检查，但它是一个适配层：
将可支持字段转换为 legacy `sys_clone()` 参数。`set_tid`/`set_tid_size` 非零会直接返回
`EINVAL`；`pidfd` 非零时先执行用户指针可读性检查，但 pidfd 功能本身仍因
`CLONE_PIDFD` 校验失败而不可用；`CLONE_INTO_CGROUP` 等 cgroup 请求也会被 flags
校验拒绝。因此不能将 `clone3` 记为完整实现。

#figure(
  flow((
    [*解析并校验 flags*],
    [*按 flags 共享或复制进程资源*],
    [*创建 TCB、内核栈和 trap context*],
    [*登记 TID 与 Process 关系*],
    [*放入 feature-selected ready queue*]
  )),
  caption: [clone 创建路径。]
)

== 映像替换：execve

`sys_execve()` 从当前地址空间复制路径、`argv` 与 `envp`。空 `envp` 采用内核提供的
竞赛运行环境；路径按当前 `FSInfo.cwd` 归一化，`/proc/self/exe` 被解析为当前可执行
文件。入口检查可执行权限和写打开冲突，然后读取 ELF 所需的头、程序头和可加载字节。

若目标不是 ELF，入口会解析 shebang：解释器及其可选参数被重建到 `argv` 前部，脚本的
绝对路径成为解释器参数；`/bin/sh` 与 `/bin/busybox` 按镜像兼容规则转到
`/musl/busybox`。无 ELF 且无有效 shebang 返回 `ENOEXEC`。历史 `.sh` 路径也保留了
busybox 兼容分支。

`MemorySetInner::from_elf()` 新建含内核映射的地址空间，映射所有 `PT_LOAD` 段，并按
ELF 标志设置用户 R/W/X 权限。若存在 `PT_INTERP`，内核还会在固定偏移映射动态解释器，
并把用户入口设为解释器入口；动态链接和共享库重定位仍由用户态解释器完成。加载器在
ELF 末尾预留 guard page 后建立初始 brk 区域，并生成 `AT_PHDR`、`AT_PHENT`、
`AT_PHNUM`、`AT_ENTRY`、`AT_BASE`、`AT_PAGESZ` 等 auxv 项。

`TaskControlBlock::exec()` 在切换地址空间前处理 `clear_child_tid`，以保证该用户地址
仍属于旧映像；之后替换 `MemorySet` 和 `SigTable`，重新分配用户栈/trap 资源，关闭
`close_on_exec` 文件描述符，清空线程的 pending/mask 信号状态，并在新用户栈写入
字符串、指针数组、auxv、随机区与 `argc`。它还将当前线程的 `comm` 更新为 `argv[0]`
的末级名称（最多 16 个字符），并在 `CLONE_VFORK` 场景恢复等待的父线程。

#figure(
  sequence(((
    [用户态], [路径、argv、envp], [sys_execve]),
    ([sys_execve], [检查文件、shebang 与 ELF 映像], [VFS / ELF loader]),
    ([TaskControlBlock], [替换映像、重建栈并返回用户态], [新程序])
  )),
  caption: [execve 当前实现路径。]
)

== 退出、僵尸与等待

`sys_exit()` 进入 `exit_current_and_run_next()`，仅终止当前线程；
`sys_exit_group()` 先通过 `exit_current_group_and_run_next()` 设置一次线程组退出码，
向其他线程组成员发送 `SIGKILL`，并唤醒其中的阻塞任务，然后使每个线程最终走自己的
退出路径。`suspend_current_and_run_next()` 也会检测线程组已经退出，防止成员在时钟
切换后继续执行。

当前线程退出时，内核依次执行 `CLONE_CHILD_CLEARTID` 写零和 futex 唤醒、robust futex
死者处理、vfork 父线程唤醒、trap context 映射移除和 `Zombie` 标记；随后从所属
`ProcessMeta.tasks` 移除自身的弱引用。它还会唤醒阻塞的兄弟线程，并从相关 futex
等待队列删除条目，避免后续 futex 唤醒或超时把已经清理的线程再次入队。

当最后一个线程退出时，退出路径冻结使用量，清理 trap area、地址空间数据、文件描述符
与文件系统上下文、POSIX 文件锁和文件租约，调用 `exit_and_reparent()` 将仍存活的
子进程优先改挂到父系中最近的存活 `child subreaper`，没有时才回退到 PID 1，并按
子进程的 `exit_signal` 通知父进程和唤醒 `child_exit_event`。已经是 zombie 的被收养
子进程也会通知新的父进程。`Process` 本身在此时仍保留在全局 PID 映射中，以供父进程
观察 zombie；真正删除发生在成功的等待调用中。

`sys_waitpid()` 支持 POSIX PID 选择语义（指定 PID、当前/指定进程组、任意子进程），
并处理 `WNOHANG`、`WNOWAIT`、`WUNTRACED`/`WSTOPPED`、`__WALL` 与 `__WCLONE`。
`sys_waitid()` 使用相同的子进程筛选和等待机制，额外报告 `WEXITED`、`WSTOPPED`、
`WCONTINUED`，以 `siginfo_t` 返回可见状态。无匹配子进程返回 `ECHILD`；允许阻塞时，
调用者在 `ProcessMeta.child_exit_event` 注册 waker，退出、stop 或 continue 事件会令其
重新扫描。消费退出事件且未指定 `WNOWAIT` 时，父进程累计子进程的资源使用量、从
`children` 删除弱引用，再调用 `Process::remove_from_global_map()` 回收进程记录。

#figure(
  flow((
    [*线程 exit / exit_group*],
    [*清理线程资源并成为 Zombie*],
    [*最后线程：通知父进程、reparent 子进程*],
    [*waitpid / waitid 报告状态*],
    [*非 WNOWAIT：从 PID 映射移除*]
  )),
  caption: [退出到父进程回收的生命周期。]
)

== 初始进程与当前边界

`INITPROC` 是惰性初始化的 `Arc<TaskControlBlock>`。首次访问时，内核从根文件系统
打开 `/initproc`，读取 ELF 并调用 `TaskControlBlock::new()`；该函数创建 PID/TID 1 的
`Process`、带标准输入输出的 `FdTable`、初始 `FSInfo`、用户地址空间、四页内核栈及
初始 trap context。`add_initproc()` 把它放入当前调度策略的就绪队列和 TID 映射，之后
`run_tasks()` 才能首次进入用户态。孤儿进程会在父进程退出时重新挂到 PID 1。

#table(
  columns: (1.6fr, 3fr),
  table.header([*边界*], [*当前状态*]),
  [调度策略], [默认启用简化 CFS；`make SCHEDULER=rr` 编译 FIFO RR。两个 feature 互斥，不能在运行时切换。],
  [ID 生命周期], [`TidHandle` 当前不归还 ID，避免 zombie 等待期间发生 PID/TID 重用。],
  [clone3], [仅将支持的 `clone_args` 字段转换到 `sys_clone`；set_tid、pidfd 和 cgroup 等扩展未实现。],
  [ELF 动态加载], [内核映射 `PT_INTERP` 指定的解释器并提供 auxv；共享库解析与重定位在用户态完成。],
  [Linux 调度 ABI], [`sched_setscheduler`/`sched_getscheduler` 等仍为兼容实现，不代表完整 `SCHED_OTHER`/实时类语义。],
  [多核], [CFS 使用所有 Hart 共享的就绪堆，并按线程 CPU affinity 过滤当前 Hart 不可运行的任务；RR 使用全局 FIFO 队列并按 `scheduled_hart` 过滤。任务 affinity 可允许任务在多个在线 Hart 上运行，已有远程入队和 IPI/空闲 Hart 通知，但尚无完整主动迁移、work stealing 或负载均衡策略。TID 映射仍为全局锁保护结构。],
)
