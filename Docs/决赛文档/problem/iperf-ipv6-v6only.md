# iperf IPPROTO_IPV6/IPV6_V6ONLY 兼容修复

## 背景

RISC-V 预赛镜像中 `iperf_testcode.sh` 会先以 daemon 模式启动 `iperf3 -s -p 5001 -D`，随后分别运行 musl/glibc 两组 UDP/TCP、并发和反向测试。`iperf3` server 在监听阶段可能通过 `getaddrinfo(AF_UNSPEC)` 获取 IPv6 wildcard 地址，并在 bind 前调用：

```c
setsockopt(s, IPPROTO_IPV6, IPV6_V6ONLY, &opt, sizeof(opt))
```

其中 Linux 上 `IPPROTO_IPV6=41`，`IPV6_V6ONLY=26`。

## 现象

用户提供的 `log.ans` 中，musl/glibc 两组 iperf 在开始阶段均出现：

```text
[WARN] [sys_setsockopt] unknown protocol! level = 41, optname = 26
iperf3: error - unable to connect to server ...: Connection refused
====== iperf BASIC_UDP end: fail ======
...
====== iperf REVERSE_TCP end: fail ======
```

所有客户端连接都被拒绝，说明 5001 端口没有成功进入监听状态。

## 分析

对照 iperf 源码 `iperf_tcp.c`，server 创建监听 socket 后会先设置 `SO_REUSEADDR`，随后如果得到的是 `AF_INET6` socket 且运行在默认 `AF_UNSPEC` 模式，会设置 `IPV6_V6ONLY=0`，用于允许 IPv4 client 通过 v4-mapped 语义连接同一个 IPv6 listener。该 `setsockopt` 失败时，iperf 会关闭 socket 并返回 `IEV6ONLY`，daemon server 随即退出。

Ya2yOS 当前 `sys_socket()` 已接受 `AF_INET6` 并复用 TCP/UDP socket 实现，`sockaddr_in6` 也能被解析为 `core::net::SocketAddr::V6`。但 `sys_setsockopt()` 只处理 `SOL_SOCKET`、`IPPROTO_IP`、`IPPROTO_TCP`，遇到 `IPPROTO_IPV6` 直接返回 `ENOPROTOOPT`，使 iperf server 初始化失败。

## 根因

`sys_setsockopt()` 缺少 `IPPROTO_IPV6/IPV6_V6ONLY` 兼容分支。对当前测试来说，iperf 设置的是 `IPV6_V6ONLY=0`，目标只是确保 IPv6 listener 不拒绝 IPv4 loopback client。内核现有网络栈已经把 `AF_INET6` socket 接入同一套 TCP/UDP 实现，但 socket option 层没有承认该 Linux 选项。

## 修复

修改 `os/src/syscall/net/consts.rs`，新增：

```rust
pub const IPPROTO_IPV6: u32 = linux_raw_sys::net::IPPROTO_IPV6 as u32;
```

修改 `os/src/syscall/net/opt.rs`：

- 引入 `IPV6_V6ONLY` 常量。
- `sys_setsockopt()` 对 `IPPROTO_IPV6/IPV6_V6ONLY` 解析 4 字节 bool 参数并返回成功。
- `sys_getsockopt()` 对同组选项返回默认 `0`，对应 Linux 默认 dual-stack 行为。

当前实现没有新增完整 IPv6-only 状态位，也没有实现真正的 IPv6-only 过滤。这个选择是有意的：本次失败路径只需要兼容 `IPV6_V6ONLY=0`，而 Ya2yOS 现有网络栈本身还没有完整 dual-stack policy；贸然加入一个未接入 bind/connect/listen 匹配路径的状态位，反而容易制造“设置成功但语义不一致”的复杂度。

## 涉及文件

- `os/src/syscall/net/consts.rs`
- `os/src/syscall/net/opt.rs`

## 验证

已执行：

```text
make
```

结果：默认 RISC-V 构建通过，仅有既有 warning。

直接执行 `make run` 时，当前沙箱环境下 QEMU `-snapshot` 会尝试在只读 `/var/tmp` 创建临时文件：

```text
Could not open temporary file '/var/tmp/vl.*': Read-only file system
```

因此使用 `/tmp` 下的 qcow2 overlay 指向原始 raw 测试镜像，并不带 `-snapshot` 直接运行 QEMU：

```text
qemu-img create -f qcow2 -F raw -b .../sdcard-rv.img /tmp/iperf-sdcard-rv-overlay.qcow2
timeout 180s qemu-system-riscv64 ... \
  -drive file=/tmp/iperf-sdcard-rv-overlay.qcow2,if=none,format=qcow2,id=x0 \
  ... > /tmp/iperf-ipv6-v6only-fix.log 2>&1
```

关键结果：

```text
#### OS COMP TEST GROUP START iperf-musl ####
====== iperf BASIC_UDP end: success ======
====== iperf BASIC_TCP end: success ======
====== iperf PARALLEL_UDP end: success ======
====== iperf PARALLEL_TCP end: success ======
====== iperf REVERSE_UDP end: success ======
====== iperf REVERSE_TCP end: success ======
#### OS COMP TEST GROUP END iperf-musl ####

#### OS COMP TEST GROUP START iperf-glibc ####
====== iperf BASIC_UDP end: success ======
====== iperf BASIC_TCP end: success ======
====== iperf PARALLEL_UDP end: success ======
====== iperf PARALLEL_TCP end: success ======
====== iperf REVERSE_UDP end: success ======
====== iperf REVERSE_TCP end: success ======
#### OS COMP TEST GROUP END iperf-glibc ####
shutdown!
```

验证日志中未再出现 `unknown protocol`、`Connection refused`、panic、`TFAIL` 或 `TBROK`。

未执行 LoongArch64 QEMU 验证；本次失败日志和触发脚本均基于当前默认 RISC-V 预赛镜像。
