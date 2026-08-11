#import "../diagrams.typ": flow, relation, sequence

= 信号机制

本章对应 `os/src/signal/`、`os/src/syscall/signal.rs`、`os/src/trap/mod.rs` 与任务模块
中的等待/状态转换逻辑。信号动作是进程级资源，pending 集、掩码、等待中断标记和备用
信号栈是线程级状态；递送在返回用户态前完成。

== 模块与数据模型

#table(
  columns: (1.45fr, 3fr),
  table.header([*位置*], [*当前职责*]),
  [`signal/types.rs`], [Linux 编号、`SigSet`、用户 ABI `SigAction`/`SigInfo`、备用栈和默认动作。],
  [`signal/action_table.rs`], [`SigTable`：线程组共享的每信号 disposition 与用户 action。],
  [`signal/delivery.rs`], [投递、UID/会话权限检查、线程组和进程组选择、唤醒 stopped/blocked 任务。],
  [`signal/pending.rs`], [选择未屏蔽 pending 信号并执行默认动作或 handler 分发。],
  [`signal/frame.rs`], [用户栈/备用栈信号帧构造及 `rt_sigreturn` 恢复。],
  [`signal/timer.rs`], [`ITIMER_*` 过期时的信号投递。],
  [`syscall/signal.rs`], [`rt_sigaction`、mask、等待、发送和返回相关 syscall 的 ABI 检查。],
)

`SigSet` 是一个 `usize` 位图，定义了 1--31 号标准信号、`SIGRTMIN` 和一个项目使用的
实时扩展位。每个 `Process` 持有 `SigTable`；表项将用户可见的 `SigAction` 与内部
`SigDisposition::{Default, Ignore, Handler}` 分开保存。默认动作不能用内核函数指针编码，
因此用户通过 `SIG_DFL` 查询或恢复 action 时仍看到 Linux ABI 规定的值。

`TaskControlBlockInner` 保存 `sig_pending`、`sig_pending_info`、`sig_mask`、
`sig_eintr`、`SignalStack` 和等待中断状态。标准信号不排队：重复投递保留原 pending
位和第一次保存的 `SigInfo`；因此自定义 `SA_SIGINFO` handler 可观察到最初发送者的
PID 和 real UID。

#figure(
  relation((
    [*SigTable（进程级）*\action/disposition],
    [*TCB（线程级）*\pending、mask、SigInfo、alt stack],
    [*trap_return*\选择信号、默认动作或信号帧],
    [*用户 handler*\ `rt_sigreturn` 恢复上下文]
  )),
  caption: [信号状态的归属与递送边界。],
)

== 发送、权限与唤醒

`kill`、`tkill` 和 `tgkill` 均校验信号编号；`kill(pid, 0)` 只进行存在性和权限探测。
`kill` 支持指定线程组、当前/指定进程组和所有可访问进程的选择语义，`tkill` 指向线程，
`tgkill` 同时验证目标 TID 隶属于指定 TGID。用户态发送路径比较发送者的 real/effective
UID 与目标 real/saved UID；effective UID 为 0 可越过普通比较，同一 session 的
`SIGCONT` 也允许发送。成功的用户态投递生成带发送者 PID/UID 的 `SigInfo`。

内核内部路径（页故障、子进程状态变化、定时器）使用不带用户发送者信息的 helper。
`add_signal_with_info()` 将信号置为 pending 后按状态处理：`SIGCONT` 或 `SIGKILL`
可将 `Stopped` 任务恢复到 `Ready`；可中断阻塞任务优先经 `interrupt_waker` 唤醒，否则
重新加入 ready queue。对默认可忽略的 `SIGCHLD`，只有安装 handler 时才作为可中断
等待处理，避免无意义地使阻塞 syscall 返回 `EINTR`。

退出和等待的通知状态存于 `ProcessMeta`，而非 `SigTable`。最后一个线程退出时会向父
线程组发送 `SIGCHLD` 并唤醒 `child_exit_event`；stop/continue 事件同样可唤醒
`waitpid`/`waitid` 的重新扫描。

== 递送与默认动作

`trap_return()` 在恢复用户寄存器前循环检查 `sig_pending & !sig_mask`，每轮选择编号
最小的信号。若本轮建立用户 handler 帧则立即返回用户态，让 handler 先运行，避免
连续递送覆盖尚未恢复的信号帧。`SIGKILL` 与 `SIGSTOP` 始终从掩码移除，
`rt_sigaction` 也拒绝修改二者的 disposition。

#table(
  columns: (1.4fr, 3fr),
  table.header([*disposition*], [*递送结果*]),
  [`Handler`], [在当前用户栈或启用的备用栈构造帧，跳转到 `sa_handler`。],
  [`Ignore`], [消费 pending 位，不改变用户执行流。],
  [`Default: Terminate/CoreDump`], [以 `128 + signo` 调用任务退出路径；当前不生成 core 文件。],
  [`Default: Stop`], [任务进入 `Stopped`，等待 `SIGCONT` 或 `SIGKILL`。],
  [`Default: Continue`], [恢复停止任务并记录可供父进程等待的 continue 事件。],
)

来自用户页故障的不可恢复访问通常转为 `SIGSEGV`；文件 mmap 的映射时 EOF 之外页面
转为 `SIGBUS`。非法指令当前直接走任务退出路径，未支持的 trap 仍会 panic，不能表述为
所有硬件异常都已具备完整 POSIX 信号语义。

== 用户信号帧与返回

当 action 为 handler 时，`setup_frame()` 先选择可用的备用栈（`SA_ONSTACK` 且栈已
启用）或当前用户栈，检查空间与用户地址有效性，再保存 machine context、旧掩码和
`SigInfo`。随后它根据 `SA_SIGINFO` 设置 handler 参数，按照 `SA_NODEFER` 更新当前
掩码，并将返回地址设置为 libc restorer 或架构提供的 `sigreturn_trampoline`。
`SA_RESETHAND` 在进入 handler 时将该 action 复位为默认；`SA_RESTART` 的可重启判断
由帧恢复路径结合原 syscall context 处理。

`sys_rt_sigreturn()` 调用 `restore_frame()`，从受检用户内存读取信号帧，恢复保存的
trap context、信号掩码和备用栈状态，并返回原始 `a0`。帧解析失败返回 `EFAULT`，避免
直接信任用户提供的栈内容。最新 trampoline 调整后，restorer 地址、用户态返回入口和
架构 trap context 必须作为一个 ABI 整体核对：RISC-V 和 LoongArch 的低层布局不同，
但高层 action、pending 与恢复协议共用。

#figure(
  sequence(((
    [内核/用户发送者], [置 pending、记录 `SigInfo`、唤醒目标], [目标 TCB]),
    ([`trap_return`], [选择未屏蔽信号并构造 frame], [用户 handler]),
    ([用户 handler], [`rt_sigreturn`], [恢复 trap context 与 mask])
  )),
  caption: [信号从投递到用户态恢复的当前路径。],
)

== 相关系统调用与限制

`rt_sigaction` 使用架构相关的 `RawSigAction` 布局与用户态交换 action；
`rt_sigprocmask`、`rt_sigpending`、`rt_sigsuspend` 和 `rt_sigtimedwait` 操作线程级
pending/mask。`rt_sigtimedwait` 在用户指定集合中消费最小编号 pending 信号，并通过
任务定时器返回 `EAGAIN`；`rt_sigsuspend` 在收到未屏蔽信号后恢复旧 mask 并返回
`EINTR`。`sigaltstack`、`setitimer`/`getitimer` 和 POSIX timer 的实现分别位于 signal
和 timer 路径。

当前限制包括：标准信号不维护 Linux realtime queue；core-dump 默认动作只终止；
`signalfd4` 仍是兼容性 fd，不能替代完整的 signal-fd 消费语义；未支持的硬件 trap
可能 panic。修改该路径时必须避免持有 task/process 锁跨越调度和用户内存访问，并验证
阻塞 syscall 的 `EINTR`、`SA_RESTART` 与 stop/continue 交互。

#text(size: 8.5pt, fill: rgb("536471"))[_实现追溯：_ `os/src/signal/`、`os/src/syscall/signal.rs`、`os/src/trap/mod.rs`]
