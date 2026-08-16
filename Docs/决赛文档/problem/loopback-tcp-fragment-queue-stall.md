# Loopback TCP 分片队列耗尽导致 iperf 卡死

## 背景

初赛镜像的 `iperf_testcode.sh` 在同一内核内通过 `127.0.0.1:5001` 依次运行
UDP、单连接 TCP、五连接 TCP 和反向 TCP。内核为 loopback TCP 设置
`LOOPBACK_TCP_MSS = 4096`，以提高本地吞吐；该大小超过路由器和环回设备的
1500-byte IP MTU，因此一个 TCP 段会拆成多个 IPv4 分片。

## 现象

`log.ans` 中 `basic`、`busybox`、`lua`、iperf 的基础/并发 TCP 与反向 UDP
均已完成，但停在：

```text
====== iperf REVERSE_TCP begin ======
Connecting to host 127.0.0.1, port 5001
Reverse mode, remote host 127.0.0.1 is sending
[  5] local 127.0.0.1 port 49164 connected to 127.0.0.1 port 5001
```

之后没有吞吐统计、`iperf Done.` 或测试组结束标记，内核没有 panic。该问题具有
时序性：同一源码的部分运行可以通过，部分运行在此处永久停止推进。

## 分析

`LoopbackDevice`、`Router::rx_buffer` 和 `Router::tx_buffer` 共用
`SOCKET_BUFFER_SIZE` 作为 IP 包槽位数。原值为 64，而一次 64 KiB TCP
发送最多形成 16 个 4096-byte 段，即约 48 个 IPv4 分片；同一轮还会并发产生
ACK 和控制包。接收任务未及时运行时，环回 FIFO 会满，`LoopbackDevice::send`
只能警告并丢弃后续分片。

同时，性能改造把 `smoltcp::Interface::poll` 拆分成 ingress/egress 循环时遗漏了
`poll_maintenance()`。该阶段负责清理过期 IP 分片重组状态；丢失的分片保留其
重组槽位会放大后续重传的资源压力，使反向 TCP 更容易无法恢复。

## 根因

环回路径按 IP 包计数的队列深度没有覆盖 4096-byte MSS 的分片突发，导致高吞吐
反向 TCP 静默丢失分片；拆分 smoltcp 轮询流程后又漏掉分片维护阶段，令丢失后的
重组状态无法按协议栈设计及时回收。两者组合表现为无 panic 的等待卡死。

## 修复

- `os/src/net/consts.rs`：将内部路由器和环回 IP 包队列从 64 提升到 256 槽，
  为数据分片和 ACK 留出空间。
- `os/src/net/service.rs`：在每轮自定义 ingress/egress 轮询前调用
  `self.iface.poll_maintenance(timestamp)`，恢复原 `Interface::poll` 的维护语义。

未修改 iperf 脚本、超时、并发度或用户态配置。

## 验证

- `make`：RISC-V64 和 LoongArch64 release 构建均通过；仅有既有
  `smoltcp` unused import/dead-code warning。
- `make run`（RISC-V QEMU）：musl 与 glibc 的 `REVERSE_TCP` 均输出完整
  2 秒吞吐统计、`iperf Done.` 和 `end: success`，随后进入两轮 `netperf` 及
  `cyclictest`。为限制完整 preliminary 套件耗时，在确认进入后续 `cyclictest`
  后人工终止 QEMU，未将整轮初赛套件标为完成。
