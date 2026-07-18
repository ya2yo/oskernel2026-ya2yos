# RISC-V 双 hart netperf 锁序与丢唤醒修复

## 背景

RISC-V QEMU 开启两个 hart 后，独立进程按 PID 分配到各自的 `home_hart`。`netserver` 与 `netperf` 因而会从两个 hart 并发进入同一个 smoltcp 协议栈、监听表、异步 I/O 执行器和计时器。

本问题是在双 hart bring-up 已能启动和运行 `iperf` 后，对并发阻塞与高频短连接路径的进一步修复。

## 现象

原始 `log.ans` 中 OpenSBI 报告两个 hart，`UDP_STREAM`、`TCP_STREAM`、`UDP_RR` 和 `TCP_RR` 可以完成，但日志停在：

```text
====== netperf TCP_CRR begin ======
MIGRATED TCP Connect/Request/Response TEST ...
```

没有 panic、`TFAIL` 或 `TBROK`。增加诊断输出或通过 GDB 暂停后，卡点会漂移到 `TCP_STREAM`、`UDP_RR` 或 `TCP_RR`，说明它不是固定的 TCP_CRR 协议错误，而是由时序触发的共享等待竞态。

## 分析

### 网络锁序

协议栈轮询原本形成以下锁链：

```text
poll_interfaces:
SERVICE -> SOCKET_SET -> LISTEN_TABLE[port]
```

旧的监听查询和 TCP 建连路径存在反向顺序：

```text
accept/can_accept:
LISTEN_TABLE[port] -> SOCKET_SET

TcpSocket::connect/bind/new_connected:
SOCKET_SET -> SERVICE
```

两个进程位于不同 hart 时，这两组顺序可分别形成 ABBA。监听 socket 的 `unlisten()` 还会在端口锁内析构队列项，析构过程再次获取 `SOCKET_SET`。

### Future 丢唤醒

修正网络锁序后，测试不再固定停在 TCP_CRR，但仍会偶发停在不同 I/O 阶段。通用 `block_on()` 存在跨核 lost-wakeup：

```text
wake hart                       waiter hart
---------                       -----------
woke = true
释放 woke 锁
                                消费 woke，重新 poll
                                Future 再次返回 Pending
检查到任务仍为 Running
不加入 ready queue
                                发布 TaskStatus::Blocked
                                永久休眠
```

关键细节是旧代码的 `*self.woke.lock() = true` 在语句末尾立即释放临时 guard；waker 写标志和检查 `TaskStatus` 并不是一个原子握手。网络 `PollSet` 会频繁唤醒多个 waiter，因此 RR 和 CRR 压力路径很容易命中该窗口。

`TaskControlBlock::poll_interrupt()` 还有同类通知窗口：先检查 `interrupted`，后注册 `AtomicWaker`。若计时器或信号在两步之间触发，producer 当时没有可唤醒对象，consumer 随后会注册并返回 `Pending`。

### 计时器锁放大

两个 hart 的空闲调度循环都在分配一个全量任务 `Vec` 并扫描所有 blocked task，即使任务只能在自己的 `home_hart` 运行。GDB 栈显示两个 hart 会同时在该扫描和 timer-wheel 到期清理中竞争全局堆锁。

timer-wheel 还在持有全局 mutex 时直接调用任务 waker，使 timer 锁临界区继续进入任务锁、ready queue 和分配器。这不是唯一的 lost-wakeup 根因，但会扩大时序窗口和锁竞争。

## 根因

本次卡死由三类单核假设叠加：

- 网络代码没有统一 `SERVICE`、`SOCKET_SET` 和监听端口锁的全局顺序。
- `block_on()` 把“消费 pending wake”和“发布 Blocked 状态”拆成了可被另一 hart 穿插的两步，waker 端也过早释放握手锁。
- blocked timer 扫描没有遵守进程的 hart 归属，并在每次调度循环中产生不必要的全局分配；timer-wheel 在锁内执行外部 waker。

TCP_CRR 的高频 `connect/accept/send/recv/close` 只是最容易放大这些窗口的负载，并非临时端口耗尽或 musl 专属协议语义问题。

## 修复

- 新增 `with_service_and_socket_set()`，所有同时访问协议栈和 socket set 的路径统一为 `SERVICE -> SOCKET_SET`。
- `Service::register_waker()` 接收已持有的 `SocketSet`，不再自行反向获取全局 socket 锁。
- TCP `connect/bind/new_connected` 拆开原有嵌套临界区；需要同时访问接口 context 和 socket 时使用统一 helper。
- `can_accept/accept` 使用 `SOCKET_SET -> LISTEN_TABLE[port]` 同序检查和摘取 handle，既消除 ABBA，也避免锁外裸 handle 被删除或复用。
- `unlisten()` 在释放端口锁后才析构监听队列。
- `MyWaker` 的阻塞侧和唤醒侧都保持 `woke -> task.inner` 锁序；pending wake 直接触发下一轮 poll，未发生 wake 时先原子发布 `Blocked` 再切回调度器。
- `poll_interrupt()` 改为先注册 `AtomicWaker`，再检查并消费 `interrupted`。
- blocked timer 仅由任务的 `home_hart` 驱动，并通过无分配遍历访问任务表；timer-wheel 只在锁内摘取到期项，在锁外调用 waker。

## 涉及文件

- `os/src/net/mod.rs`
- `os/src/net/service.rs`
- `os/src/net/general.rs`
- `os/src/net/listen_table.rs`
- `os/src/net/tcp.rs`
- `os/src/task/future/mod.rs`
- `os/src/task/future/time.rs`
- `os/src/task/manager.rs`
- `os/src/task/task/task.rs`

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过，仅有既有 vendored `smoltcp` warning 和 Cargo 配置弃用提示。
- `make build-arch TARGET_ARCH=loongarch64`：通过，warning 同上。
- 两次独立执行 `timeout 300s make run > log.ans 2>&1`，均选中 boot hart 0 并完成双 hart 启动。
- 两次有效运行中，musl 与 glibc 的 `UDP_STREAM`、`TCP_STREAM`、`UDP_RR`、`TCP_RR`、`TCP_CRR` 共 10 项全部 `end: success`，两组均打印 `GROUP END`，末尾为 `shutdown!`。
- 最终 `log.ans` 无内核 panic、`Connection refused`、`TFAIL` 或 `TBROK`。glibc 的 `enable_enobufs failed: getprotobyname` 是既有用户态提示，不影响各项成功结果。

## 剩余风险

OpenSBI 偶尔会选择 hart 1 作为 boot hart；该启动样本会在 `task::add_initproc` 附近出现 `Lazy instance has previously been poisoned` panic，尚未进入 netperf。它是独立的启动问题，本次没有用延长超时或忽略日志的方式掩盖，最终验证只统计完成全局初始化的 boot hart 0 样本。

网络服务当前仍只保留一个绑定到最后注册任务的协议超时 Future；物理网卡 IRQ waker、SYN 重传去重和 TCP close/TIME_WAIT 回收也需要后续独立加固。这些路径未在本次 loopback netperf 日志中表现为直接根因，因此未扩大本补丁范围。
