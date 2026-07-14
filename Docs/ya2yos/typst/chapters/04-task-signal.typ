= 任务、调度与信号

== 进程和线程模型

任务子系统以 `TaskControlBlock` 表示可调度线程，以进程对象保存线程组元数据和共享资源。线程拥有 tid、内核栈、trap context、任务上下文、状态及阻塞信息；进程拥有 pid、地址空间、fd 表、文件系统上下文、信号动作及子进程/退出相关状态。资源通过 `Arc` 共享，`ResourceSlot` 仅保护资源指针本身，调用方取得 `Arc` 后必须立即释放 slot 锁。

`clone`/`clone3` 根据 flag 决定共享或复制地址空间、文件、信号处理和线程组关系；`execve` 替换映像；`exit`/`exit_group` 负责线程组终止、资源清理和父进程可观察的 wait status；`wait*` 接口回收已退出子进程。

== 调度与阻塞

`TaskManager` 维护就绪任务，`Processor` 保存每核当前任务。任务状态包括 Ready、Running、Blocked、Stopped 和 vfork 等待状态。时钟中断或主动让出时，当前线程保存上下文并调用架构 `__switch`；阻塞对象被唤醒后将线程重新放入 ready 队列。

futex、管道、socket、poll/epoll 等路径使用阻塞与唤醒机制协作。实现规则是先更新等待状态和队列，再切换调度；唤醒方必须在状态改变后放回 ready 队列。不能在拿着 fd 表、地址空间、信号表或 futex 队列锁时进入可能阻塞的路径。

== 信号生命周期

信号模块由 `types`、`action_table`、`pending`、`delivery`、`frame` 和 `timer` 组成。发送路径将信号加入线程或线程组的 pending 集合；用户态来源还保存发送者 pid/uid 等 `siginfo`。在 trap 返回用户态或阻塞点被打断时，内核选择一个未屏蔽待处理信号并按 disposition 分发。

#table(
  columns: (1.15fr, 3fr),
  table.header([*disposition*], [*行为*]),
  [自定义 handler], [构造用户栈上的 signal frame，设置用户 PC 到 handler],
  [`SIG_IGN`], [消费并丢弃可忽略信号；`SIGKILL` 与 `SIGSTOP` 不可设置为忽略],
  [默认终止/停止/继续], [更新任务或线程组状态，必要时生成可由 `wait` 观察的状态],
  [`sigreturn`], [验证并恢复保存的寄存器和信号掩码],
)

`SA_RESTART` 影响可重启系统调用的返回方式；实时等待接口如 `rt_sigtimedwait` 从 pending 队列取出信号并向用户返回 siginfo。定时器模块可将到期事件转换为信号投递。
