# LoongArch CAgent loopback TCP 分片校验和与大请求传输

## 背景

final-2026 的 CAgent 在同一内核内启动 `agent_lite` 和本地 HTTP 服务端，二者通过
`127.0.0.1:8080` 通信。服务端对请求体只执行一次 `read()`，随后解析 HTTP JSON 并返回
推理结果。测试脚本和镜像均为只读，本问题只能在内核网络栈中修复。

## 现象

维护者提供的 LoongArch64 `log.ans` 已完整执行 CAgent 组，但其中三项被拒绝：

```text
testcase cagent kernel reject 2553
testcase cagent fs-readwrite reject 5574
testcase cagent fs-directory reject 7538
```

拆分诊断后，`fs-readwrite` 和 `fs-directory` 在第二轮 HTTP 请求处稳定复现
`Error: No choices in response`。带 TCP trace 的日志显示两条应用层请求分别为 1517 B 和
1510 B；在 1500 B IPv4 MTU 下，它们被拆为 `1480 + 57` 和 `1480 + 50` 字节的 IPv4
payload。接收端的 fragment assembler 已收到两片，但没有向 TCP socket 交付数据，发送端
随后重传同一 TCP 段。

## 分析

`Router` 同时服务 loopback 和物理 VirtIO 网卡，必须继续向 smoltcp 报告 1500 B MTU；全局
提高 MTU 会让外部以太网路径错误地产生超 MTU 帧。局部 loopback 则可以使用较大的 TCP MSS，
由 IPv4 分片在 Router/loopback 间传输并在对端重组，使服务端的一次 `read()` 能看到完整的
局部 HTTP 请求。

启用 smoltcp IPv4 fragmentation 后进一步定位到发送端校验和错误。`dispatch_ip()` 在产生
首片前把完整 TCP datagram 存入 `Fragmenter::buffer`，但旧代码调用：

```rust
emit_ip(&ip_repr, &mut frag.buffer);
```

`frag.buffer` 长度为 8192 B。`IpPayload::Tcp` 将 IP header 后的整个切片包装为
`TcpPacket`，`TcpRepr::emit()` 因而把未使用的 buffer 尾部也计入 TCP pseudo-header length
和 checksum。接收端重组后只得到实际 TCP datagram 长度，校验和必然不匹配，`TcpRepr::parse()`
丢弃报文。这解释了“IPv4 已重组、TCP 却未收到”的 trace。

分片支持还暴露两个并发/旁路边界：

- smoltcp `Interface` 只有一个全局 `Fragmenter`。十项 CAgent 并发启动时，未完成的分片包若
  继续扫描其他 socket，会覆盖该 buffer。
- Router 的监听 SYN 窃听不能把 IPv4 首片当作完整 TCP SYN。首片仍可能带 `MF=1`，未完成重组
  且不在该旁路检查 TCP checksum，可能提前分配被动连接资源。

## 修复

- `os/Cargo.toml` 启用 IPv4 fragmentation，配置 8192 B fragmentation buffer 和 16 个重组槽。
- `os/vendor/smoltcp/src/socket/tcp.rs` 增加受控的 `local_mss` override；未设置时仍按接口 MTU
  计算。TCP 数据段、有效 MSS 和 SYN/SYN-ACK MSS option 使用同一值。
- `os/src/net/tcp.rs` 和 `os/src/net/listen_table.rs` 仅对 IPv4 loopback endpoint 设置
  `LOOPBACK_TCP_MSS = 4096`，并关闭 Nagle 与 delayed ACK。外部网卡和 IPv6 socket 不受影响。
  `send()` 在字节进入 smoltcp 发送队列后主动轮询接口，保证本地阻塞读者可观察到已排队的数据。
- `os/vendor/smoltcp/src/iface/interface/mod.rs` 把 TCP emit 的 buffer 限制为真实
  `total_ip_len`：

  ```rust
  emit_ip(&ip_repr, &mut frag.buffer[..total_ip_len]);
  ```

  同时在未排空 fragmenter 时优先继续发送当前报文、停止扫描其他 socket，并在 TX 队列满时返回
  `PollResult::None`，避免内部空转。
- `os/src/net/router.rs` 忽略所有 IPv4 分片的监听 SYN 窃听，并使用
  `TcpPacket::new_checked()` 解析完整的未分片 TCP 报文。
- 新增 smoltcp 单元测试，构造大于 MTU 的 TCP datagram，捕获发送片、走真实 IPv4 重组并断言
  TCP checksum 校验后产生 RST。旧的全 buffer checksum 实现会在该断言前丢弃报文。

## 涉及文件

- `os/Cargo.toml`
- `os/src/net/consts.rs`
- `os/src/net/listen_table.rs`
- `os/src/net/router.rs`
- `os/src/net/tcp.rs`
- `os/vendor/smoltcp/src/iface/interface/mod.rs`
- `os/vendor/smoltcp/src/iface/interface/tests/ipv4.rs`
- `os/vendor/smoltcp/src/socket/tcp.rs`

`user/src/bin/initproc.rs` 的临时诊断入口已恢复为正式 `test_final_2026()`；维护者已有的
BuildStorm 注释保持不变，未改动 CAgent 测试脚本或镜像。

## 验证

执行：

```text
cargo test --offline test_tcp_tx_fragmentation_checksum_after_reassembly
make build-arch TARGET_ARCH=loongarch64
timeout 300s make run TARGET_ARCH=loongarch64 > /tmp/cagent-loongarch64-final.log 2>&1
python3 scripts/judge_cagent-glibc.py < /tmp/cagent-loongarch64-final.log
make build-arch TARGET_ARCH=riscv64
```

新增 smoltcp 单元测试通过。最终 LoongArch64 日志包含 `#### OS COMP TEST GROUP END cagent ####`
和 `shutdown!`，判题脚本对以下十项均返回 `pass: 1/1`：

```text
factorial, date, network, cpu, kernel,
fs-create, fs-readwrite, fs-directory, fs-search, fs-usage
```

最终 RISC-V release 构建通过；修复过程中还执行过完整 RISC-V CAgent 回归，十项均通过。没有
出现 `Invalid JSON`、`No choices in response`、`TFAIL`、`TBROK` 或 panic。
