# netperf glibc: 12865 控制端口残留监听

## 背景

full run 末尾按顺序执行：

```rust
run_testsuit("musl\0", "netperf_testcode.sh\0");
run_testsuit("glibc\0", "netperf_testcode.sh\0");
```

`netperf_testcode.sh` 固定使用 loopback 和控制端口 `12865`：

```sh
ip="127.0.0.1"
port=12865
./netserver -D -L $ip -p $port &
server_pid=$!
...
kill -9 $server_pid
```

## 现象

用户反馈 full run 最后的 `netperf-glibc` 失败，内核日志出现：

```text
socket already listening on port 12865
```

后续 `riscv.ans` 确认：在不改测试脚本、不更换端口的情况下，`netperf-musl` 与 `netperf-glibc` 都使用原始 `netperf_testcode.sh`，并都固定绑定 `12865`。

## 分析

该错误来自 `os/src/net/listen_table.rs`：

```rust
warn!("socket already listening on port {port}");
Err(SysErrNo::EADDRINUSE)
```

只有 `LISTEN_TABLE` 中对应端口仍有监听项时，新的 `listen()` 才会返回 `EADDRINUSE`。`TcpSocket::Drop` 会调用 `shutdown(Shutdown::Both)`，监听状态下会执行：

```rust
LISTEN_TABLE.unlisten(self.bound_endpoint()?.port);
```

因此理论上只要持有监听 socket 的 `netserver` 进程真正退出，12865 端口应当被释放。

`riscv.ans` 进一步显示，musl 版 netserver 在 `kill -9` 后确实进入退出路径并被 wait 回收；临时把 glibc 脚本端口改成 `12866` 也能跑通，说明网络收发本身不是根因，失败集中在固定端口复用时旧监听项未及时从 `LISTEN_TABLE` 删除。

关键点在进程退出清 fd 的语义：旧路径在 `FdTable::clear()` 中直接清空 `files`，监听端口释放依赖 `Arc<Socket>` 最后一个引用 drop 后触发 `TcpSocket::Drop -> shutdown(Shutdown::Both) -> LISTEN_TABLE.unlisten()`。如果退出瞬间仍有 fd 表副本、轮询路径或临时 `Arc` 持有该 socket，fd table 已经清空但监听表仍可能短暂保留旧端口，下一轮原始 glibc netserver 立即 `listen(12865)` 时就会遇到 `EADDRINUSE`。

## 根因

进程退出时清空 fd table 只释放 fd 表持有的 `Arc`，没有主动关闭 socket；监听 socket 的全局端口释放过度依赖最后一个 `Arc` drop。对 TCP listening socket 来说，这会让 `LISTEN_TABLE` 的生命周期长于进程 fd 生命周期，导致 netperf 两轮固定复用 `12865` 时发生残留监听。

## 修复

在 `os/src/fs/fstruct.rs` 中让进程退出清 fd 时主动关闭 socket：

```rust
fn close_on_process_exit(&self) {
    if let FileClass::Socket(socket) = &self.file {
        let _ = socket.0.shutdown(Shutdown::Both);
    }
}

pub fn clear(&self) {
    let mut inner = self.get_mut();
    for desc in inner.files.iter().flatten() {
        desc.close_on_process_exit();
    }
    inner.files.clear();
}
```

这样进程退出清 fd 时，即使 socket 还有其它临时 `Arc` 引用，监听状态也会立即执行 `shutdown(Shutdown::Both)`，从 `LISTEN_TABLE` 删除端口。后续 drop 再次 shutdown 是幂等路径，不影响普通 fd 引用生命周期。

同时撤销测试入口中的临时脚本绕过，`user/src/bin/initproc.rs` 恢复为直接运行原始 glibc netperf：

```rust
run_testsuit("musl\0", "netperf_testcode.sh\0");
run_testsuit("glibc\0", "netperf_testcode.sh\0");
```

## 涉及文件

- `os/src/fs/fstruct.rs`
- `user/src/bin/initproc.rs`

## 验证

已执行：

```text
make
timeout 300s make run > riscv.ans 2>&1
```

结果：

- 当前默认 `TARGET_ARCH=riscv64`，构建通过。
- `riscv.ans` 中 `netperf-musl` 与 `netperf-glibc` 均使用原始 `netperf_testcode.sh`，两轮都固定使用 `12865`。
- 两轮各 5 个子项均输出 `end: success`：`UDP_STREAM`、`TCP_STREAM`、`UDP_RR`、`TCP_RR`、`TCP_CRR`。
- 日志末尾输出 `#### OS COMP TEST GROUP END netperf-glibc ####` 和 `shutdown!`，未再出现 `socket already listening on port 12865`。

## 2026-07-03 重构后回归

### 背景

`9fa9699d2580a7de85e41baf2e49c3e51000d6f0` 之后，内核进行了大规模资源与锁边界重构：`FdTable` 从旧的进程内部锁结构中拆出，成为独立同步对象；fd 的关闭、替换、进程退出清理也改为通过 `FdTable` 自己的 API 管理。这个重构本身是为了缩短 PCB 锁持有时间，但也改变了 socket fd 生命周期代码的实际触发位置。

重构前，`netperf` 已经可以在原始脚本下连续执行：

```rust
run_testsuit("musl\0", "netperf_testcode.sh\0");
run_testsuit("glibc\0", "netperf_testcode.sh\0");
```

重构后，原来能通过的 `netperf` 出现两类回归：

1. 第一份 `log.ans` 中，`netperf-musl` 的 `UDP_STREAM` 建立控制连接后马上读到 EOF：

   ```text
   recv_response: partial response received: 0 bytes
   ```

   随后 `netserver` 不再监听，后续四项都无法建立控制连接。

2. 修复第一类问题的初版补丁后，`netperf-musl` 五项恢复 success，但 `netperf-glibc` 启动时又回到旧问题：

   ```text
   socket already listening on port 12865
   Unable to start netserver with  '127.0.0.1' port '12865'
   ```

### 新日志分析

带 syscall trace 的 `log.ans` 显示第一类失败并不是 `connect()` 没连上：

```text
[PID 5] TCP connection from 127.0.0.1:49152 to 127.0.0.1:12865
[PID 5] Connect ret = 0
[PID 4] Pselect6 ret = 1
[PID 4] Accept ret = 4
```

也就是说客户端 PID 5 已经连到 `netserver` 父进程 PID 4，PID 4 也 `accept()` 得到了控制连接 fd 4。随后 `netserver` fork 出 PID 6 处理本次 `UDP_STREAM`，父进程关闭 accepted fd 并继续监听：

```text
[PID 4] Clone ret = 6
[PID 4] Close ret = 0
[PID 4] Pselect6 ...
```

客户端后续通过控制连接发送 656 字节请求成功：

```text
[PID 5] SendTo ret = 656
[PID 5] Pselect6 ret = 1
[PID 5] RecvFrom ret = 0
recv_response: partial response received: 0 bytes
```

`pselect6` 返回可读但 `recvfrom()` 返回 0，说明控制连接被对端关闭，而不是等待超时。结合 PID 4 在 fork 后立刻 `close(4)`，可以定位到：父进程关闭自己的 accepted fd 时，内核把底层 TCP socket 也 `shutdown(Shutdown::Both)` 了，导致子进程 PID 6 继承到的 fd 还在，但对应 socket 已经被父进程关闭。

### 新根因

重构后的 `FdTable::close()` / `FdTable::set()` 使用了“同一个 fd table 内是否还有 alias”判断：

```rust
let should_close = !desc.has_fd_alias(&inner.files);
if should_close {
    desc.close_socket();
}
```

这个判断只能发现同一张 fd table 内的 `dup()` alias，发现不了 `fork()` 后父子进程各自 fd table 中共享的同一个 `Arc<Socket>`。因此 `netserver` 父进程在 fork 后关闭 accepted fd 时，当前 fd table 内确实没有其它 alias，于是主动 `shutdown()` 了 socket；但从 Linux fd 语义看，子进程继承的 fd 仍引用同一个 open file description，父进程关闭自己的 fd 不能关闭子进程仍持有的连接。

第一版修复把 `close()` 收窄为只有 `Arc::strong_count(socket) == 1` 时才主动 `shutdown()`，这修好了控制连接 EOF，但也把 6.30 的旧问题带回来了：`FdTable::clear()` 在进程退出时遇到临时 `Arc` 引用时不再主动释放监听 socket，导致 `LISTEN_TABLE` 中的 12865 项残留，`netperf-glibc` 无法重新绑定固定端口。

最终根因是 `close(fd)` 和进程退出清理 fd table 被混成了同一种 socket 关闭语义：

- 普通 `close(fd)`：只能释放本 fd 引用；若同一个 socket 还被 fork 后的子进程或临时对象引用，不能主动 shutdown。
- 进程退出 `FdTable::clear()`：需要主动关闭本进程持有的 socket，尤其是 listening socket，确保全局 `LISTEN_TABLE` 立即释放端口，不能依赖最后一个临时 `Arc` drop。

### 本次修复

`os/src/fs/fstruct.rs` 将两种语义拆开：

```rust
fn close_socket_if_last_ref(&self) {
    if let FileClass::Socket(socket) = &self.file {
        if Arc::strong_count(socket) == 1 {
            let _ = socket.0.shutdown(Shutdown::Both);
        }
    }
}

fn shutdown_socket(&self) {
    if let FileClass::Socket(socket) = &self.file {
        let _ = socket.0.shutdown(Shutdown::Both);
    }
}
```

具体策略：

- `FdTable::close()` / `FdTable::set()`：只在当前 `Socket` 没有其它 `Arc` 引用时 `shutdown()`，避免父进程关闭 accepted fd 时误关子进程继承的控制连接。
- `FdTable::clear()`：进程退出清理 fd table 时无条件对 socket 执行 `shutdown(Shutdown::Both)`，恢复 6.30 修复的端口释放语义，确保 listening socket 立即从 `LISTEN_TABLE` 删除。

同时保留本轮排查中发现的两个 syscall 语义修正：

- `os/src/syscall/net/io.rs`：`sys_recvmsg()` 使用内核临时缓冲接收后，将实际接收内容按 iovec 分段复制回用户态，避免只更新内核缓冲但用户 iovec 仍为空。
- `os/src/syscall/net/socket.rs`：`accept4()` 写回用户态地址时使用 `peer_addr()`，并传播 `write_to_user()` / `fd_table.set()` 错误，不再静默忽略失败。

### 本次验证

已执行：

```text
make
timeout 300s make run > log.ans 2>&1
```

结果：

- 当前默认 `TARGET_ARCH=riscv64`，构建通过。
- `log.ans` 中 `netperf-musl` 五项全部 success：

  ```text
  ====== netperf UDP_STREAM end: success ======
  ====== netperf TCP_STREAM end: success ======
  ====== netperf UDP_RR end: success ======
  ====== netperf TCP_RR end: success ======
  ====== netperf TCP_CRR end: success ======
  ```

- `log.ans` 中 `netperf-glibc` 同样使用原始 `12865` 端口启动 netserver，五项全部 success：

  ```text
  Starting netserver with host '127.0.0.1' port '12865' and family AF_UNSPEC
  ====== netperf UDP_STREAM end: success ======
  ====== netperf TCP_STREAM end: success ======
  ====== netperf UDP_RR end: success ======
  ====== netperf TCP_RR end: success ======
  ====== netperf TCP_CRR end: success ======
  #### OS COMP TEST GROUP END netperf-glibc ####
  shutdown!
  ```

- 未再出现 `recv_response: partial response received: 0 bytes`、`socket already listening on port 12865`、`Unable to start netserver`、panic 或 QEMU 异常退出。
