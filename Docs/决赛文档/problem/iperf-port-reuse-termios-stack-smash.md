# iperf musl/glibc 连续运行端口复用与 glibc stack smashing

## 背景

当前 RISC-V 测试入口连续执行标准 testsuit helper：

```rust
run_testsuit("musl\0", "iperf_testcode.sh\0");
run_testsuit("glibc\0", "iperf_testcode.sh\0");
```

`iperf_testcode.sh` 在 musl 和 glibc 目录中都固定使用 loopback `127.0.0.1` 和端口 `5001`，并通过：

```sh
$iperf -s -p $port -D
```

启动 daemon server。脚本本身没有在结束时 kill server，因此两轮固定端口连续运行时必须保证上一轮 daemon 不会继续占用 `5001`。

## 现象

早期失败日志显示 `iperf-musl` 通过后，进入 `iperf-glibc` 时出现：

```text
socket already listening on port 5001
*** stack smashing detected ***: terminated
```

修复 glibc `TCGETS` 栈破坏后，最新 `log.ans` 仍在 `iperf-glibc` 组开头出现：

```text
#### OS COMP TEST GROUP START iperf-glibc ####
socket already listening on port 5001
====== iperf BASIC_UDP begin ======
...
====== iperf REVERSE_TCP end: success ======
```

这不是无害 WARN。glibc server 的 `listen(5001)` 已经失败，后续 glibc client 之所以继续 success，是因为它们连接到了上一轮 `iperf-musl` 残留的 daemon server。

## 分析

端口释放与 `netperf-glibc-port-reuse.md` 的方向一致：进程退出、显式 close、close-on-exec 清理 fd 时，不能只依赖 `Arc<Socket>` 最后一次 drop，应该在 fd 生命周期结束时主动 `shutdown(Shutdown::Both)`，让 TCP listening socket 立即执行 `LISTEN_TABLE.unlisten(port)`。

但 iperf 比 netperf 多一个 daemon 生命周期问题。`iperf3 -s -D` 会脱离脚本继续运行；`run_testsuit()` 只等待脚本进程退出，不会自动终止脚本启动后被 init 收养的后台 daemon。因此 musl 组结束后，旧 server 仍然是 init 的子进程，并继续监听 `5001`。

上一轮尝试把 `SO_REUSEADDR` 处理成“新 listener 覆盖旧监听表项”后，虽然 `socket already listening` 消失，但 glibc 仍失败，并新增 `accept before listen`。这说明简单替换监听表会破坏旧/新 server 的状态，不符合 Linux 对 active listener 的语义，也不是正确修复。

另一个独立问题是 glibc `iperf3` 的栈保护失败。用 `make log` 复现并查看 syscall trace，第一次栈保护失败前的关键路径为：

```text
[syscall begin] Ioctl
[sys_ioctl] fd=1, cmd=21505
[syscall ret --- OK] Ioctl ret = 0
[syscall begin] Writev
*** stack smashing detected ***: terminated
```

`21505 == 0x5401`，即 `TCGETS`。当 glibc `iperf3` 打印结果前查询 stdout termios 时，内核的 stdio ioctl 把自定义 `RawTermios` 直接写回用户栈。旧结构包含 `c_cc[32]`、padding 和 `c_ispeed/c_ospeed`，总计 60 字节；但 Linux `TCGETS` 使用 old kernel termios 布局，RISC-V 上应为 36 字节。多写的 24 字节覆盖了 glibc 栈上的 canary，于是后续进入 `__stack_chk_fail`。

## 根因

- `LISTEN_TABLE.listen()` 不应在 `SO_REUSEADDR` 下覆盖已有 listener；覆盖会让旧 server 的 `accept()` 与监听表状态分裂。
- fd close/exit 清理路径若不主动 shutdown socket，listener 释放会过度依赖最后一个临时 `Arc` drop。
- `iperf3 -s -D` daemon 会在脚本结束后被 init 收养并继续监听 `5001`；标准 `run_testsuit()` 原先只等待脚本进程，没有清理该类 testsuit 残留后台子进程。
- `TCGETS` 返回结构使用了过大的自定义 termios 布局，向 glibc 用户栈多写，触发 stack smashing。

## 修复

- 撤销失败的 `SO_REUSEADDR` 覆盖监听表实验，`LISTEN_TABLE.listen()` 对已有端口继续返回 `EADDRINUSE`。
- `FdTable::clear()` / `close()` / `close_on_exec()` 在 fd 生命周期结束时主动对 socket 调用 `shutdown(Shutdown::Both)`，并在 fd alias 存在时避免过早 shutdown。
- `sys_close()` 和 `sys_close_range()` 改用 `FdTable::close()`，让显式 close socket 也释放 listener。
- `/proc/<pid>/stat` 和 `/proc/<pid>/status` 使用 `ProcessMeta.comm` 生成进程名，动态刷新时也写入真实 comm。
- `RawTermios` 改为 Linux old kernel termios 布局：`c_iflag/c_oflag/c_cflag/c_lflag`、`c_line`、`c_cc[19]`，总计 36 字节；`TCGETS/TCSETS*` 不再读写 `c_ispeed/c_ospeed`。
- 保留 blocked 任务收到可终止信号时通过 `wake_interruptible()` 唤醒的修复，使 `SIGKILL` 能打断阻塞中的 server。
- 保持 iperf 调用使用标准 `run_testsuit(root, script)`，不引入 `run_iperf_testsuit()`；在通用 `run_testsuit()` 收尾阶段调用 `kill_processes(-1, SIGKILL)`，再循环 `wait()` 回收被 init 收养的 testsuit 残留子进程。
- 用户态 `sys_kill` 包装支持负 pid，新增 `kill_processes(isize, signum)`；内核 `kill(-1, sig)` 路径跳过 pid 1，避免从普通进程调用时误伤 init。

## 涉及文件

- `os/src/net/listen_table.rs`
- `os/src/net/tcp.rs`
- `os/src/fs/fstruct.rs`
- `os/src/syscall/fs/fd_ops.rs`
- `os/src/fs/kernel_fs_ops/proc_file.rs`
- `os/src/task/task/task.rs`
- `os/src/fs/files/stdio.rs`
- `os/src/signal/mod.rs`
- `user/src/syscall/mod.rs`
- `user/src/lib.rs`
- `user/src/bin/initproc.rs`

## 验证

已执行：

```text
make
timeout 180s make run > /tmp/iperf-generic-cleanup.log 2>&1
```

结果：

- `make` 通过，当前默认架构为 RISC-V。
- 沙箱内直接 `make run` 因 QEMU 需要写 `/var/tmp/vl.*` 失败；已在非沙箱环境重跑同一命令完成验证。
- `/tmp/iperf-generic-cleanup.log` 中 `iperf-musl` 六项均为 `success`。
- `/tmp/iperf-generic-cleanup.log` 中 `iperf-glibc` 六项均为 `success`。
- `iperf-glibc` 组开始后直接进入 `====== iperf BASIC_UDP begin ======`，未再出现 `socket already listening on port 5001`。
- 日志未出现 `replace reusable listening socket`、`accept before listen`、`Connection refused`、`*** stack smashing detected ***` 或 kernel panic，最后正常 `shutdown!`。
