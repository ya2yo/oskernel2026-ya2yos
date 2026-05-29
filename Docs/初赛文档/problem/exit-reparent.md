# 进程退出托孤 exit_and_reparent

### 背景

父进程在子进程尚未 `wait` 时退出，子进程应被挂到 initproc（pid 1），否则子进程的 `ppid` 与 `children` 链表会指向已回收的父进程。

### 现象

部分 fork/wait 相关测例异常：僵尸进程无法被正确回收，或 initproc 收不到 `SIGCHLD`。

### 分析

进程最后一个线程退出时，原实现只回收本进程资源，未把仍存活的子进程从父进程 `children` 移出并挂到 initproc。

### 修复

在 `Process` 中实现 `exit_and_reparent`（`os/src/task/process/process.rs`）：

1. 收集尚未 wait 的子进程
2. 清空本进程 `children` / `tasks`
3. 将每个子进程的 `parent_pid` 改为 1，并加入 initproc 的 `children`
4. 向 initproc 发送 `SIGCHLD`

在 `exit_current_and_run_next`（`os/src/task/mod.rs`）中，当进程所有线程均为 zombie 时调用。

### 涉及文件

- `os/src/task/process/process.rs`
- `os/src/task/mod.rs`

### 验证

libctest、libcbench 相关 wait 测例通过。

---
