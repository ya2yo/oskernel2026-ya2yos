# 实现过程遇到的问题

## sys_clone的实现

通过查阅资料，sys_clone就是fork在riscv平台上的实现。sys_clone具体实现：
> The clone() wrapper function creates a new process by invoking the clone system call. The child process starts by calling the function fn with the argument arg. The stack argument specifies the location of the stack used by the child process. The flags argument is a bit mask that specifies what is shared between the calling process and the child process. The low byte of flags contains the number of the termination signal sent to the parent when the child dies. The remaining arguments (parent_tid, tls, child_tid) are optional and are used to store the thread ID of the child in the parent and child processes, respectively, and to specify the address of a new TLS area for the child process.
> The clone3() system call provides a superset of the functionality of the older clone() interface. It also provides a number of API improvements, including: space for additional flags bits; cleaner separation in the use of various arguments; and the ability to specify the size of the child’s stack area. The cl_args argument is a pointer to a structure of type struct clone_args. The size argument is the size of this structure. As with fork(), clone3() returns in both the parent and the child. It returns 0 in the child process and returns the PID of the child in the parent.

下面是手册上写的关于CloneFlags的字段内容:

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
