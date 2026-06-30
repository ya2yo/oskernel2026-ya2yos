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
