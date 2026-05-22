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

[32m[DEBUG] [HART0] [PID 4] [TID 5] [(Weak), (Weak), (Weak)][0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] futex_wake_up_bitset: wake 1 threads[0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] [syscall ret --- OK] Futex ret = 1[0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] 222 return_to_user, trap_cx.sepc=0x150003b368, sp=0x2a23422998, kstack=0xffffffc0805ff000, trap_cx=0xffffffc0836aa000[0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] 111 trap_handler: scause=Exception(Syscall), stval=0x0, sepc=0x150006093c[0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] [syscall begin] Accept sepc = 0x1500060940[0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] sys_accept <= fd: 3, flags: 0[0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] [block_on] strong count: 3[0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] [block_on] Pending strong_count = 4[0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] [block_current_and_run_next()] BEGIN![0m
[32m[DEBUG] [HART0] [PID 4] [TID 5] [processor]: take_current_task![0m

Pending 代码块里面的强引用导致计数异常，会导致死循环。
