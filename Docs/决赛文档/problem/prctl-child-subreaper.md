# prctl PR_SET_CHILD_SUBREAPER 子进程收养语义

## 背景

Linux 的 `prctl(PR_SET_CHILD_SUBREAPER, arg2)` 使调用进程成为 child subreaper。其后代的直接父进程
退出时，孤儿应被最近仍存活的祖先 subreaper 收养；只有不存在 subreaper 时才由 PID 1 收养。

Ya2yOS 已有 `prctl(167)` 分发，但原 `PR_SET_CHILD_SUBREAPER` 分支只返回成功，`exit_and_reparent()`
也始终把子进程交给 PID 1。因此调用返回 0 后，用户态仍无法观察到 subreaper 语义。

## 实现范围

- `PR_SET_CHILD_SUBREAPER = 36`：将任意非零 `arg2` 设为 true，零清除标记；未使用的其余参数不作
  额外校验，和 Linux 的 `!!arg2` 语义一致。
- `PR_GET_CHILD_SUBREAPER = 37`：向 `arg2` 指向的用户态 `int` 写入 0 或 1；无效地址由
  `copy_to_user_val()` 返回 `EFAULT`。
- 标记保存于 `ProcessMeta`，而非单线程 `TaskControlBlockInner`。因此同一线程组共享该状态，非线程
  fork/clone 的新 `Process` 默认不继承，`execve` 保留已有进程元数据。

## 收养路径

进程完成 group exit 时，`Process::exit_and_reparent()` 先快照并清空原父进程的 `children`。随后从
退出进程的 `parent_pid` 开始逐级查找：第一个 `is_child_subreaper` 且尚未设置
`group_exit_code` 的祖先成为新父；查找失败则回退 PID 1。

每个被收养子进程都会更新 `parent_pid`，并加入新父的 `children`。既有 `waitpid()` 本来只扫描该
列表，因此不需要为 subreaper 新增单独的等待路径。重父化时将 child 的 `exit_signal` 设为
`SIGCHLD`，使默认 `waitpid()` 也能回收原先带其他 clone exit signal 的进程。

如果被收养者已经是 zombie，新父会收到 `SIGCHLD` 且其 `child_exit_event` 被唤醒；仍在运行的
被收养者不产生伪造信号。这样中间子进程退出和孙进程随后退出恰好各贡献一次 `SIGCHLD`。

## 验证

执行：

```bash
make TARGET_ARCH=riscv64 build-arch
timeout 120s make run TARGET_ARCH=riscv64
```

构建通过。QEMU 正常输出 `shutdown!`，musl 与 glibc 的 LTP `prctl03` 均为：

```text
passed   6
failed   0
broken   0
```

该用例覆盖 `PR_SET` 成功、`PR_GET` 的 0/1 值、fork 后标记不继承、孤儿进程 PPID 指向
subreaper、subreaper 能 `wait()` 回收孤儿，以及 orphan 退出通知。

## 未验证边界

- 按维护者要求，本项未进行 LoongArch64 运行时验证。
- Ya2yOS 当前未实现 PID namespace、ptrace 重父化等 Linux 相关分支，本轮没有扩展这些能力。
- 未单独构造多层 subreaper、并发退出或 `PR_SET_PDEATHSIG` 联动用例；当前 LTP `prctl03` 覆盖单层
  subreaper 的主路径。

## 涉及文件

| 路径 | 修改内容 |
| --- | --- |
| `os/src/syscall/sys/prctl.rs` | SET/GET option 的 syscall 层参数与用户内存处理。 |
| `os/src/task/process/process.rs` | 进程级状态及 orphan reparenting 选择、链接和通知。 |
| `Docs/决赛文档/problem/README.md` | 新增本文索引。 |
