# libcbench-musl的修复过程

背景：Tatlin原来的内核版本在进行musl/libcbench测试的时候运行到 b_pthread_createjoin_serial1 (0) 这个位置时没有 time 的输出。个人推测应该是Tatlin并没有实现 /proc/<pid>/ 目录的创建机制。在本人做完ltp的测试后，突然发现这个测试在运行到b_pthread_create_serial1 (0)时会直接卡死，但是make log后的输出结果虽说不正确，但是并不会卡死，只是速度非常慢。

## 修复方法

一下修复方法来自gpt5.5, 本人在尝试deepseekv4修复多个小时无果后在gpt5.5的帮助下终于明白问题所在。
首先，根据AI总结的linux7.0里面copy_process的行为中，并不会像我之前修改的那样在创建进程时直接创建这个目录，而是实行懒创建，即按需创建，我原来的修改导致每次创建新的进程都会创建这个目录，会拖慢速度(ps: 现在也是)，最主要的原因是clone不应该对线程也去创建这个目录。
其次，在我原来的实现里面，对于已经失效的兄弟线程，我没有去释放，但是根据gpt5.5修改给的方案是在创建线程时清理失效的兄弟线程，避免高频 pthread_create 后任务表膨胀。
最后，gpt5.5还修改了线程退出时，往curr_task_inner.clear_child_tid里面写入四个字节的0而不是一个字节，因为clear_child_tid是usize类型的，这样改确实有助于与内核的健壮性。而且在线程退出时，进程任务标直接移除当前的线程。

涉及文件：

- os/src/task/task/task.rs:563：线程创建时清理 process.meta.tasks 里的失效 weak 引用，避免高频 pthread_create 后任务表膨胀。
- os/src/task/task/task.rs:635：CLONE_THREAD 不再重复创建 /proc/<pid>/stat 和 /proc/<pid>/maps。之前每次 pthread 都走文件系统 open/create 路
径，是 b_pthread_create_serial1 极慢的主因。

- os/src/task/mod.rs:210：CLONE_CHILD_CLEARTID 退出时改为写 4 字节 u32 0，不再只清 1 字节。
- os/src/task/mod.rs:261：线程退出后从进程任务列表移除当前 TCB weak 记录。
