# cyclictest STRESS_P1 hackbench ready Broken pipe 修复

## 背景

`cyclictest_testcode.sh` 的 `STRESS_P1/STRESS_P8` 会先后台启动：

```text
./hackbench -l 100000000 &
```

随后再运行 `cyclictest`。`hackbench` 在 process mode 下会创建大量 worker，并使用 `AF_UNIX socketpair()` 作为父进程和 worker 之间的 ready 同步通道。

本次问题出现在内核资源与 fd table 重构之后。重构前部分 fd 分配路径依赖“分配后立即占位”的行为；重构后 `FdTable::alloc_fd()` 只返回当前最小空闲 fd，不会自动修改 fd table。因此，调用者如果连续调用两次 `alloc_fd()` 而没有先 `set()` 第一个 fd，就会两次拿到同一个 fd。

## 现象

新的 `log.ans` 中，`cyclictest STRESS_P1` 前后出现大量：

```text
CLIENT: ready write (error: Socket not connected)
CLIENT: ready write (error: Broken pipe)
```

`cyclictest` 自身仍能输出 `STRESS_P1 end: success`，但压力进程并没有真正按预期建立完整的 worker 同步通道。Linux 样例里正常允许在 kill 阶段出现 `SENDER: write ... Broken pipe`，但这里的错误发生在 `CLIENT: ready write` 阶段，说明 `hackbench` worker 在启动握手时同步 socket 已经断开。

## 分析

日志显示 `hackbench` 启动后先输出：

```text
Running in process mode with 10 groups using 40 file descriptors each (== 400 tasks)
Each sender will pass 100000000 messages of 100 bytes
```

随后 worker 一启动就写 ready fd，并立刻得到 `ENOTCONN/EPIPE`。这说明问题集中在 `socketpair()` 返回的两个 fd，而不是 `cyclictest` 的调度、定时器或 signal 逻辑。

检查 `sys_socketpair()` 发现，代码先连续分配两个 fd，再把两个 socket 端点写入 fd table：

```rust
let fd1 = fd_table.alloc_fd()?;
let fd2 = fd_table.alloc_fd()?;
fd_table.set(fd1, sock1)?;
fd_table.set(fd2, sock2)?;
```

在重构后的 `FdTable::alloc_fd()` 语义下，第一次 `alloc_fd()` 不会占用槽位，因此第二次 `alloc_fd()` 仍返回同一个最小空闲 fd。结果是：

1. `socketpair()` 返回给用户态的 `sv[0]` 和 `sv[1]` 实际相同。
2. 第二次 `set(fd2, sock2)` 覆盖第一次 `set(fd1, sock1)`。
3. 用户态按两个独立 fd 使用时，关闭其中一个“端点”会直接破坏唯一实际存在的 fd。
4. `hackbench` worker 写 ready 通道时看到 peer 不存在或通道已关闭，于是打印 `CLIENT: ready write (error: ...)`。

这也是为什么该问题在内核重构后重新出现：测试程序和 `socketpair()` 需求没有变化，变化的是 fd table 的分配边界。原来能通过的测试依赖的隐式占位行为，在重构后不再成立。

排查中还发现 `UnixSocket::recv()` 和 `accept()` 在队列为空时直接返回 `EAGAIN`，没有按阻塞 socket 语义等待。`hackbench` 同步通道修复后需要阻塞读语义保证父进程可以等待 worker ready，因此一并补齐 AF_UNIX socket 的 poll/waker 路径。

## 根因

根因是 `sys_socketpair()` 对 fd table 重构后的 `alloc_fd()` 语义适配不完整：连续两次分配 fd 但中间没有占位，导致两个 socketpair 端点复用同一个 fd。

连带问题是 AF_UNIX socket 缺少阻塞 `recv/accept` 语义，在队列为空时直接返回 `EAGAIN`，不适合 `hackbench` 这种用 socketpair 做同步的程序。

## 修复

涉及文件：

| 文件 | 修改 |
|------|------|
| `os/src/syscall/net/socket.rs` | `sys_socketpair()` 改为分配 `fd1` 后立即 `set()` 占位，再分配 `fd2`；若第二次分配或用户 fd 数组写回失败，主动关闭已安装的 fd，避免半初始化泄漏 |
| `os/src/net/unix.rs` | 为 AF_UNIX socket 增加 `recv_poll/accept_poll`，`recv()` 和 `accept()` 通过 `poll_io()` 支持阻塞等待；`send/connect/shutdown/drop` 在状态变化时唤醒等待者 |

关键语义拆分：

- `socketpair()` 两端必须是两个不同 fd，且各自指向不同的 UNIX socket 端点。
- 阻塞 socket 上的 `read/recv/accept` 不能在暂时无数据时立即向用户态返回 `EAGAIN`。
- `MSG_DONTWAIT` 或 `O_NONBLOCK` 仍保留非阻塞 `EAGAIN` 行为。

## 验证

已执行：

```text
make
timeout 300s make run > log.ans 2>&1
git diff --check
```

结果：

- 当前默认 `TARGET_ARCH=riscv64`，`make` 通过。
- `log.ans` 中不再出现 `CLIENT: ready write`。
- `log.ans` 中不再出现 `Socket not connected`、`Transport endpoint is not connected` 或 ready 阶段 `Broken pipe`。
- `cyclictest-musl` 四项均 success：
  - `NO_STRESS_P1`
  - `NO_STRESS_P8`
  - `STRESS_P1`
  - `STRESS_P8`
- `cyclictest-glibc` 四项均 success。
- musl/glibc 两轮均输出 `====== kill hackbench: success ======`。
- 日志末尾正常 `shutdown!`。

未执行 `TARGET_ARCH=loongarch64` 验证；本次复现与修复在当前默认 RISC-V 配置下完成。
