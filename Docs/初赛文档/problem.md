# 开发过程遇到的问题汇总

## sys_clone行为

在riscv里面fork这个系统调用已经被clone取代
> The clone() wrapper function creates a new process by invoking the clone system call. The child process starts by calling the function fn with the argument arg. The stack argument specifies the location of the stack used by the child process. The flags argument is a bit mask that specifies what is shared between the calling process and the child process. The low byte of flags contains the number of the termination signal sent to the parent when the child dies. The remaining arguments (parent_tid, tls, child_tid) are optional and are used to store the thread ID of the child in the parent and child processes, respectively, and to specify the address of a new TLS area for the child process.
> The clone3() system call provides a superset of the functionality of the older clone() interface. It also provides a number of API improvements, including: space for additional flags bits; cleaner separation in the use of various arguments; and the ability to specify the size of the childâs stack area. The cl_args argument is a pointer to a structure of type struct clone_args. The size argument is the size of this structure. As with fork(), clone3() returns in both the parent and the child. It returns 0 in the child process and returns the PID of the child in the parent.

clone_args的字段结构

```c
struct clone_args {
    u64 flags;        /* Flags bit mask */
    u64 pidfd;        /* Where to store PID file descriptor (pid_t *) */
    u64 child_tid;    /* Where to store child TID, in child's memory (pid_t *) */
    u64 parent_tid;   /* Where to store child TID, in parent's memory (pid_t *) */
    u64 exit_signal;  /* Signal to deliver to parent on child termination */
    u64 stack;        /* Lowest address of stack */
    u64 stack_size;   /* Size of stack */
    u64 tls;          /* Location of new TLS */
    u64 set_tid;      /* Pointer to a pid_t array (since Linux 5.5) */
    u64 set_tid_size; /* Number of elements in set_tid (since Linux 5.5) */
    u64 cgroup;       /* File descriptor for target cgroup of child (since Linux 5.7) */
};
```

## pending导致死循环

[DEBUG] [HART0] [PID 4] [TID 5] [(Weak), (Weak), (Weak)]
[DEBUG] [HART0] [PID 4] [TID 5] futex_wake_up_bitset: wake 1 threads
[DEBUG] [HART0] [PID 4] [TID 5] [syscall ret --- OK] Futex ret = 1
[DEBUG] [HART0] [PID 4] [TID 5] 222 return_to_user, trap_cx.sepc=0x150003b368, sp=0x2a23422998, kstack=0xffffffc0805ff000, trap_cx=0xffffffc0836aa000
[DEBUG] [HART0] [PID 4] [TID 5] 111 trap_handler: scause=Exception(Syscall), stval=0x0, sepc=0x150006093c
[DEBUG] [HART0] [PID 4] [TID 5] [syscall begin] Accept sepc = 0x1500060940
[DEBUG] [HART0] [PID 4] [TID 5] sys_accept <= fd: 3, flags: 0
[DEBUG] [HART0] [PID 4] [TID 5] [block_on] strong count: 3
[DEBUG] [HART0] [PID 4] [TID 5] [block_on] Pending strong_count = 4
[DEBUG] [HART0] [PID 4] [TID 5] [block_current_and_run_next()] BEGIN!
[DEBUG] [HART0] [PID 4] [TID 5] [processor]: take_current_task!

Pending 代码块里面的强引用导致计数异常，会导致死循环。

## 单独运行cgroup_fj_proc 卡死

运行cgroup_fj相关测试时，由于 `FAIL LTP CASE cgroup_fj_function.sh : 2`等相关测试的失败导致cgroup_fj_proc会直接卡死，没有信号告诉这个任务退出。通过显示地调用`./cgroup_fj_function.sh cpuset` 来避免这个错误。

修改后运行panic
单独运行时最后panic`[kernel] Panicked at src/arch/riscv64/qemu/page_table.rs:293 ly: a valid pte without COW flag found at 0x0` 经过检查，发现原来的设计中是这样的
> // 在原来的写法中，不论是否有cow标志都会返回true，这里改为没有cow标志时返回false
> // 简单测试发现此处valid的pte都具有cow标志，在这放一个panic，看看未来是否会出现panic
因此此处我将其改回return false;

## 龙芯架构添加网络模块后卡在汇编阶段

根据gdb的调试结果，内核启动后卡住，然后反汇编结果如下:

```bash
   0x9000000000200178 <spin+336>:   .word       0x00000000
   0x900000000020017c <spin+340>:   .word       0x00000000
   0x9000000000200180 <spin+344>:   .word       0x00000000
   0x9000000000200184 <spin+348>:   .word       0x00000000
   0x9000000000200188 <spin+352>:   .word       0x00000000
   0x900000000020018c <spin+356>:   .word       0x00000000
   0x9000000000200190 <spin+360>:   .word       0x00000000
   0x9000000000200194 <spin+364>:   .word       0x00000000
   0x9000000000200198 <spin+368>:   .word       0x00000000
   0x900000000020019c <spin+372>:   .word       0x00000000
   0x90000000002001a0 <spin+376>:   .word       0x00000000
   0x90000000002001a4 <spin+380>:   .word       0x00000000
   0x90000000002001a8 <spin+384>:   .word       0x00000000
   0x90000000002001ac <spin+388>:   .word       0x00000000
   0x90000000002001b0 <spin+392>:   .word       0x00000000
```

首先修改了链接器，但是在网卡驱动初始化还是会出问题，所以最后选择不初始化了，未来有待改进。

## 龙芯架构 busy_box 测试失败

根据cursr分析：
已分析并修复新的 log.ans 末尾 LoadPageFault。

根因是 LoongArch 的 COW 页表标志处理不完整：fork 后私有 mmap/brk 页进入 COW 时只清了 WRITEABLE，但没有清 DIRTY；解除 COW 后也没有补回 DIRTY 并刷新 TLB。结果父子进程的私有堆/mmap 页写入隔离不可靠，busybox shell 在后续 wait4/SIGCHLD 返回后读到了被污染的 malloc/指针状态，最终访问 0x2a2343e... 触发 LoadPageFault。

修改点：

os/src/trap/mod.rs：LoongArch PageModifyFault 先尝试走 COW handler，非 COW 页才按原逻辑置 dirty。
os/src/arch/loongarch64/qemu/page_table.rs：进入 COW 时清 WRITEABLE | DIRTY；解除 COW 时恢复 WRITEABLE | DIRTY 并刷新 TLB；非 COW 情况返回 false，不再 panic。
验证：

make loongarch64-build 通过。
make run 后日志中 LoadPageFault count = 0、StorePageFault count = 0。
busybox 文件操作尾段已跑到 #### OS COMP TEST GROUP END busybox-musl ####，末尾正常 shutdown!。

## 龙芯架构 libcbench_testcode

龙芯下用户态非法指令导致内核死循环的问题。
主要改动：

- os/src/arch/loongarch64/qemu/trap_interface.rs:
  - tlb_page_modify_handler() 改为用 badv::read().vaddr()
  - 保留之前把 InstructionNotExist 映射为 IllegalInstruction 的修复
- os/src/trap/mod.rs:
  - 启用非法指令处理
  - timer 中断分支内立即重装下一次 timer
- os/src/timer.rs:
  - 恢复 TICKS_PER_SEC 频率，不再 1 秒一次 tick
- os/src/trap/trap_types.rs:
  - 新增 IllegalInstruction

## lmbench

[WARN] [HART0] [PID 4] [TID 4] [kernel] hart 0 Exception(StorePageFault) in application, bad addr = 0x2a23446000, bad instruction = 0x1b384, kernel killed it.
[WARN] [HART0] [PID 4] [TID 4] don't send SIGSEGV, just exit the process

根据 cursor 回答：这是 lmbench 的保护异常测试：它故意向只读 mmap 页写入，期望内核产生 SIGSEGV / SIGBUS，然后用户注册的 signal handler 捕获它，用来测量 protection fault 开销。

原先的机制是直接退出当前进程，但是原来的作者有实现发送信号的代码，只是没有启用，通过判断进程是否注册sig_handler来发送信号，否则还是直接exit。
改完后发现还是死循环，根据gdb的结果，sepc的值没有发生改变，推测信号处理函数异常。
继续分析后发现，`Protection fault` 本身不是最后卡住的位置。

后续继续分析发现，`Protection fault` 本身已经完成，真正卡住的是脚本下一项：

```sh
./lmbench_all lat_pipe -P 1
```

`lat_pipe` 会 fork 出父子进程，用两根 pipe 做 ping-pong 往返测试。原来的 pipe 实现中，空读或满写时只是调用 `suspend_current_and_run_next()`。这个函数只会把当前任务重新置为 `Ready`，等价于 yield，不是真正阻塞等待。结果父子进程在空 pipe 上反复进入内核轮询，`Protection fault` 后看起来像死循环，实际上是卡在 `lat_pipe` 的大量 pipe 往返中，长时间跑不出下一项。

本次修改点：

- `os/src/fs/files/pipe.rs`
  - 为 pipe ring buffer 增加 `read_waiters` 和 `write_waiters`。
  - 读空 pipe 时，把当前任务置为 `Blocked` 并放入读等待队列；写入数据后唤醒读者。
  - 写满 pipe 时，把当前任务置为 `Blocked` 并放入写等待队列；读取数据后唤醒写者。
  - 入队前后复查 pipe 状态，避免“刚检查为空/满，对端马上写入/读取”导致丢唤醒。
  - 阻塞前检查 pending signal，有信号则返回 `EINTR`，避免信号无法处理。

- `os/src/task/mod.rs`
  - 增加 `schedule_blocked_current()`，用于已经由调用者设置为 `Blocked` 的任务直接切回调度器，避免 pipe 等待队列需要重复设置状态。

- `os/src/signal/mod.rs`
  - 给阻塞态任务发送信号时，将其改回 `Ready` 并放回 ready queue。否则 `SIGKILL` 等信号可能只进入 `sig_pending`，任务仍停在 pipe/futex 等等待队列中，无法回到 trap 返回路径处理信号。

- `os/src/arch/riscv64/qemu/page_table.rs`
  - 同步 LoongArch 已修复过的 COW 处理：进入 COW 时清 `WRITEABLE | DIRTY`，解除 COW 时恢复 `WRITEABLE | DIRTY` 并刷新 TLB。
  - 这可以避免 fork 后父子进程继续通过旧 writable/dirty TLB 或页表状态写共享页，污染 busybox/lmbench 的用户态状态。

验证：

- `make build-arch TARGET_ARCH=riscv64` 通过。
- `timeout 90s make run` 中已经可以越过原先卡点，输出：

```text
Protection fault: ...
Pipe latency: ...
Process fork+exit: ...
Process fork+execve: ...
```

## riscv 测试 iozone-glibc失败

**riscv当前情况：**
\#### OS COMP TEST GROUP START iozone-glibc ####
iozone automatic measurements
[ERROR] [HART0] [PID 3] [TID 3] clockid != 0 and 1 ------------ clockid = 5
[ERROR] [HART0] [PID 3] [TID 3] clockid != 0 and 1 ------------ clockid = 5
panic
[kernel] Panicked at src/mm/translate.rs:118 called `Option::unwrap()` on a `None` value

修改：
将os/src/syscall/fs/stat.rs: 90 行附加位置调用的转换函数没有处理缺页情况，改为copy_from_user

**loongarch当前情况:**
Error relocating ./iozone: __isoc23_sscanf: symbol not found
符号链接相关本人直接询问AI结果如下：
分析结果
  根因：os/src/fs/map_dynamic_link.rs:55 行存在路径映射错误。
  错误链路
  log.ans 中的报错：
  Error relocating ./iozone:__isoc23_sscanf: symbol not found
  __isoc23_sscanf、__isoc23_strtoll、__isoc23_strtol 是 glibc 2.38 引入的 C23 标准兼容符号。LoongArch 的磁盘镜像中 glibc 版本正是
  2.38，这些符号是存在的。
  问题出在内核的动态链接器路径映射。iozone 是 glibc 编译的动态链接 ELF，其 .interp 段指定的解释器路径为
  /lib64/ld-linux-loongarch-lp64d.so.1。但内核在 map_dynamic_link.rs:55 做了错误的映射：
  // 第 55 行 — 这是 Bug！
  "/lib64/ld-linux-loongarch-lp64d.so.1" => Some("/musl/lib/libc.so"),
  这导致glibc 的二进制被 musl 的 libc.so 作为动态链接器加载。musl 不提供__isoc23_* 这些 glibc 特有的 C23 符号，所以符号解析失败。
  对比 RISC-V 为什么正常
  RISC-V 的映射是正确的（第 40 行）：
  "/lib/ld-linux-riscv64-lp64d.so.1" => Some("/glibc/lib/ld-linux-riscv64-lp64d.so.1"),
  这是将 glibc 动态链接器正确地映射到了 glibc 自己的 ld-linux-riscv64-lp64d.so.1。RISC-V 的 iozone-glibc 测试失败是另一个问题（translate.rs:118
  panic，clockid=5 导致的缺页异常，已在 problem.md 中描述），和动态链接无关。
  修复方法
  os/src/fs/map_dynamic_link.rs 需要两处改动：
  第 55 行：将映射目标从 /musl/lib/libc.so 改为真正的 glibc 动态链接器：
  "/lib64/ld-linux-loongarch-lp64d.so.1" => Some("/glibc/lib/ld-linux-loongarch-lp64d.so.1"),
  第 19 行附近：在 DYNAMIC_PATH 集合中补上 LoongArch 的 glibc 动态链接器路径：
  "/glibc/lib/ld-linux-loongarch-lp64d.so.1",

修复完后继续报错：
./iozone: error while loading shared libraries: libc.so.6: cannot open shared object file: No such file or directory
panic
[kernel] Panicked at src/mm/translate.rs:234 called `Option::unwrap()` on a `None` value
依旧是翻译机制没有考虑到缺页情况，没有调用缺页处理函数。
