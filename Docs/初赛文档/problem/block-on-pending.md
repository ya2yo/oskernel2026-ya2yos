# pending导致死循环

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
