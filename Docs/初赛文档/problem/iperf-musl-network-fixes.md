# iperf-musl 网络兼容修复

## 背景

musl `iperf_testcode.sh` 覆盖 UDP/TCP 基础、并发和反向传输场景。当前测试入口单跑：

```text
run_testsuit("musl\0", "iperf_testcode.sh\0");
```

该组测试会启动 iperf3 server，再依次执行 `BASIC_UDP`、`BASIC_TCP`、`PARALLEL_UDP`、`PARALLEL_TCP`、`REVERSE_UDP` 和 `REVERSE_TCP`。其中 `PARALLEL_UDP` 会通过 `-P 5` 建立多条 UDP stream，多条已连接 UDP socket 会共享 server 端本地端口。

## 现象

最初日志中出现过多个独立失败点：

```text
[kernel] Panicked at src/mm/address.rs:103 invalid va: 0x2000002fff447bc8
iperf3: unable to send cookie: Bad address
iperf3: unable to set socket buffer size: Invalid argument
```

前置问题修复后，`BASIC_UDP` 和 `BASIC_TCP` 已能成功，但测试卡在 `PARALLEL_UDP`：

```text
====== iperf BASIC_UDP end: success ======
====== iperf BASIC_TCP end: success ======
====== iperf PARALLEL_UDP begin ======
Connecting to host 127.0.0.1, port 5001
qemu-system-riscv64: terminating on signal 15 ... (timeout)
```

这说明剩余问题不是启动、单流 UDP 或 TCP 基础连接，而是 UDP 多流并发路径中某些 socket 长期等不到期望数据。

## 分析

本轮修复从 `log.ans` 的最后失败点反推，沿 syscall 与网络栈路径排查。

用户态 page fault panic 来自 RISC-V trap 路径对 `stval` 直接构造 `VirtAddr`。当用户态给出非规范地址时，内核应该向线程发送 `SIGSEGV`，而不是在地址类型构造处 panic。

iperf3 启动时依赖 `/dev/urandom` 生成 cookie。devfs 只有 `/dev/random`，缺少 `/dev/urandom` 注册和打开路径，会导致 iperf3 初始化失败。

`select/pselect6` 原实现复用了输入 fdset 作为输出，并按请求 fd 数量而不是实际 ready 数量计数；`FdSet` 位索引还把 `FD_SET_LEN` 当作每个 `usize` 的 bit 数。这样会把未 ready 的 socket 报告给用户态，触发 server 读写异常。

`iperf3: unable to send cookie: Bad address` 的直接原因是 `UserBuffer::read(len)` 即使实际用户缓冲区不足，也会返回长度为 `len` 的零填充 `Vec`。TCP write 路径因此可能把 37 字节 cookie 报告为写入 64 KiB，用户态继续推进指针后访问非法地址。

`SO_SNDBUF/SO_RCVBUF` 的 setsockopt 参数在 Linux ABI 中是 4 字节 `int`，原实现按 `usize` 解析，导致 iperf3 设置 socket buffer 返回 `EINVAL`。同时 syscall 分发把 `getsockopt` 第五参数当成值传递，内核实现也没有把 optval/optlen 写回用户态。

最后的 `PARALLEL_UDP` timeout 来自 UDP 入包分发。Ya2yOS 的 `UdpSocket::recv()` 已按 connected peer 过滤，但 vendored smoltcp 的 UDP ingress 只按本地端口和本地地址匹配，并把包投递给第一个匹配 socket。并发 UDP stream 下多个 socket 共享 server 端本地端口，不同 client 源端口的数据可能进入错误 socket 的 rx queue。错误 socket 在 `recv()` 中因 peer 不匹配返回 `EAGAIN`，正确 socket 又一直收不到包，最终用户态反复 `pselect/clock_gettime` 并超时。

## 根因

- RISC-V page fault 路径没有校验 `stval` 是否可表示为合法 `VirtAddr`。
- devfs 缺少 `/dev/urandom`。
- `FdSet` bit 计算错误，`pselect6` 没有生成新的 ready fdset。
- `UserBuffer::read()` 可能返回超过实际用户缓冲区长度的数据，破坏 write 的短写语义。
- `SO_SNDBUF/SO_RCVBUF` 和 `getsockopt` 的 Linux ABI 处理不完整。
- UDP 底层 ingress 分发只看本地端口，未优先匹配 connected socket 的远端四元组。

## 修复

修改 `os/src/trap/mod.rs`：

- RISC-V 与 LoongArch 均使用 `VirtAddr::try_from(stval)`。
- 非法 fault address 不再 panic，改为向当前线程发送 `SIGSEGV`。

修改 `os/src/fs/files/devfs.rs` 和 `os/src/fs/kernel_fs_ops/initfiles.rs`：

- 注册并支持打开 `/dev/random` 与 `/dev/urandom`。
- `DevRandom` 记录设备路径，使 `fstat()` 的 devno 与打开路径一致。

修改 `os/src/syscall/options.rs` 和 `os/src/syscall/io_mpx/select.rs`：

- 使用 `8 * size_of::<usize>()` 作为 fdset word bit 数。
- `got_fd()` 改为只读查询。
- `pselect6` 每轮构造新的 ready fdset，只写回实际 ready 的 fd，并按 ready 数量返回。

修改 `os/src/mm/translate.rs` 和 `os/src/syscall/fs/io.rs`：

- `UserBuffer::read(len)` 将请求长度夹到实际可读长度，并按实际拷贝长度截断返回。
- `sys_write()` 在 copy 或底层 write 已有部分进展后遇到错误时返回短写，且防御性夹紧底层 write 返回值。

修改 `os/src/syscall/net/opt.rs` 和 `os/src/syscall/mod.rs`：

- `SO_SNDBUF/SO_RCVBUF` 按 4 字节 signed `int` 解析，负数返回 `EINVAL`。
- `getsockopt` syscall 分发传入 `socklen_t *`，实现读取用户 optlen、写回 optval 和实际长度。
- 支持当前已有的 `SO_REUSEADDR`、`SO_SNDBUF`、`SO_RCVBUF`、`SO_KEEPALIVE`、超时、`IP_TTL` 和 `TCP_NODELAY` 查询。

修改 `os/vendor/smoltcp/src/socket/udp.rs`、`os/vendor/smoltcp/src/iface/interface/udp.rs` 和 `os/src/net/udp.rs`：

- smoltcp UDP socket 增加可选 `remote_endpoint`。
- UDP ingress 先查找 connected socket 的精确远端匹配，再退回普通监听 socket。
- Ya2yOS UDP `connect()` 同步设置底层 remote endpoint，`shutdown()` 清理 peer 状态。

## 涉及文件

- `os/src/trap/mod.rs`
- `os/src/fs/files/devfs.rs`
- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `os/src/syscall/options.rs`
- `os/src/syscall/io_mpx/select.rs`
- `os/src/mm/translate.rs`
- `os/src/syscall/fs/io.rs`
- `os/src/syscall/net/opt.rs`
- `os/src/syscall/mod.rs`
- `os/src/net/udp.rs`
- `os/vendor/smoltcp/src/socket/udp.rs`
- `os/vendor/smoltcp/src/iface/interface/udp.rs`
- `Docs/初赛文档/开发日志.md`
- `Docs/初赛文档/problem/README.md`
- `Docs/初赛文档/problem/iperf-musl-network-fixes.md`
- `Docs/初赛文档/ai.log`
- `Docs/初赛文档/AI_INTERACTION.md`

## 验证

已执行：

```text
make
timeout 300s make run
```

结果：

```text
====== iperf BASIC_UDP end: success ======
====== iperf BASIC_TCP end: success ======
====== iperf PARALLEL_UDP end: success ======
====== iperf PARALLEL_TCP end: success ======
====== iperf REVERSE_UDP end: success ======
====== iperf REVERSE_TCP end: success ======
#### OS COMP TEST GROUP END iperf-musl ####
shutdown!
```

`PARALLEL_UDP` 已从外部 300 秒 timeout 变为正常完成。日志中 TCP 项仍有非致命提示：

```text
iperf3: getsockopt - Protocol not available
```

该提示来自 iperf3 查询当前内核尚未支持的 TCP option；本轮验证中不影响 `iperf-musl` 六项 success，后续可单独补齐。
