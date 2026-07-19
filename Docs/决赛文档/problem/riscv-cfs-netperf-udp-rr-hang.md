# RISC-V CFS netperf UDP_RR 首 burst 卡死

## 背景

7 月 18 日的 [RISC-V 双 hart netperf 锁序与丢唤醒修复](./riscv-smp-netperf-wakeup-locking.md) 已统一网络锁序，并修复 `block_on()` 的跨 hart wakeup 握手。默认调度器切换到 per-Hart CFS 后，RISC-V 双 hart 上仍可在 `UDP_RR` 的第一轮请求/响应处永久停滞。

为避免完整 netperf 中其余四项改变时序或干扰端口状态，本次将 `initproc` 入口收敛为 musl 的单一 `UDP_RR`。测试在 `/musl` 内由 BusyBox shell 启动 `netserver`，运行原有的 loopback、端口、socket buffer 和 request/response 参数；客户端退出后 shell `kill -9` 并 `wait` server，再将客户端状态返回给 initproc。

## 现象

修复前的独立最小日志停在首个 burst：

```text
====== netperf UDP_RR begin ======
Starting netserver with host '127.0.0.1' port '12865' and family AF_UNSPEC
MIGRATED UDP REQUEST/RESPONSE TEST ... : first burst 0
qemu-system-riscv64: terminating on signal 15
```

没有 `panic`、`TFAIL`、`TBROK`、`Connection refused` 或客户端协议错误。外部 timeout 终止 QEMU 前，双方都没有打印结果表。

进程的 home hart 按 `(pid - 1) % HART_NUM` 分配，典型拓扑为：

```text
initproc  -> hart 0
shell     -> hart 1
netserver -> hart 0
netperf   -> hart 1
```

因此一次 loopback 请求和响应都会经过两个并发 hart，能够放大调度器出队、网络 waker 和协议栈 timeout 的交错。

## 分析

### CFS 陈旧 entry 的丢队列窗口

CFS 用每个 task 的原子 `on_rq` 避免重复入队。旧的 `fetch_task()` 从 heap 弹出 entry 后先读取并释放 `task.inner`，只在随后发现 entry 非 `Ready` 时才清除 `on_rq`。跨 hart 的网络 waker 可在这两步之间运行：

```text
hart A: pop stale entry, on_rq = true
        lock task.inner, observe Blocked, unlock

hart B: wake task: Blocked -> Ready
        add_task() sees on_rq = true and skips enqueue

hart A: clear on_rq and discard the stale entry
```

结果是 task 已为 `Ready`，但 queue 中没有 entry。若该 UDP socket 没有第二个事件再次唤醒 task，`netserver` 或 `netperf` 将永久无法被调度。

此前 CFS 的陈旧 entry 过滤本身是必要的，不能通过让调度器接受 `Running`/`Blocked` entry 来回避问题；正确的交接边界必须保证 waker 要么看到仍有效的 entry，要么看到已经清除的 membership 并能够补入新 entry。

### smoltcp deadline 的微秒换算错误

`Service::now()` 通过 `Instant::from_micros()` 向 smoltcp 提供单调时间，`Service::register_waker()` 则把 `iface.poll_at()` 返回的绝对微秒 deadline 转成 `Timespec` 交给 `sleep_until()`。

旧实现有两个确定的单位错误：

- `NANOS_PER_MICROS` 错写为 `1_000_000`，使 `get_time_ns() / NANOS_PER_MICROS` 实际得到毫秒，却作为微秒传给 smoltcp。
- `Timespec::from_micros()` 用 `/ 1000` 填充秒字段，并把微秒余数直接填入纳秒字段；例如 `1_000_000` 微秒被解释为 `1000` 秒而不是 `1` 秒。

这会让协议栈在收包事件未立即推进时的 deadline fallback 延迟约三个数量级。它与 CFS 丢队列窗口不是同一把锁的错误，但二者都落在 UDP 请求/响应双方等待下一次 poll/wake 的路径上。

## 修复

- `os/src/task/scheduler/cfs.rs`：在持有 `task.inner` 的同一临界区内读取 `TaskStatus` 并清除已弹出 entry 的 `on_rq`。网络 waker 随后只能看到“旧 entry 仍被声明”或“membership 已清除，可以重新入队”两种完整状态。
- `os/src/timer/mod.rs`：将 `NANOS_PER_MICROS` 更正为 `1_000`。
- `os/src/timer/timespec.rs`：`Timespec::from_micros()` 改为 `micros / 1_000_000` 秒与余数乘 `1_000` 纳秒，恢复 smoltcp absolute deadline 到内核时间的正确映射。
- `user/src/bin/netperf/`：新增 `udp_rr.rs` 独立测例模块；`initproc` 只调用该模块，避免完整 netperf 的其它 workload 干扰复现。脚本保留原 `netserver` 后台进程与 `wait` 清理语义。

本次不改 UDP 协议参数、端口号、socket buffer 大小、测试镜像或完整 netperf 脚本，也不以延长 timeout 掩盖卡死。

## 涉及文件

- `os/src/task/scheduler/cfs.rs`
- `os/src/timer/mod.rs`
- `os/src/timer/timespec.rs`
- `user/src/bin/initproc.rs`
- `user/src/bin/netperf/mod.rs`
- `user/src/bin/netperf/udp_rr.rs`

## 验证

- 修复前：`/tmp/netperf-udp-rr-cfs-before.log` 在 `first burst 0` 后被外部 timeout 终止。
- `make build-arch TARGET_ARCH=riscv64`：通过，默认 scheduler 为 CFS；只有既有 Cargo 配置弃用提示和 vendored `smoltcp` warning。
- 修复后先后取得三次独立 RISC-V 双 hart 有效样本，均输出 `====== netperf UDP_RR end: success ======` 与 `shutdown!`；速率约为 `4.3k trans/s`。
- 补回 server `wait` 后重新构建并运行，最终根目录 `log.ans` 显示 boot hart 0、`3527.82 trans/s`、测试 group END 和 `shutdown!`，未发现 `panic`、`TFAIL`、`TBROK` 或 `QEMU: Terminated`。
- `make build-arch TARGET_ARCH=loongarch64`：通过，验证共享 timer/CFS 代码可在 LoongArch64 编译；本轮未运行 LoongArch64 的 UDP_RR 行为回归。

## 剩余风险

- OpenSBI 偶尔选择 hart 1 作为 boot hart 时，启动期仍可能在 `task::add_initproc` 触发既有 `spin::Once` poisoned panic。该样本尚未进入 UDP_RR，不能作为本问题的成功或失败结论。
- `processor::run_tasks()` 仍有“将 `Running` current 入队、而 CFS/RR 只分发 `Ready` entry”的独立契约缺口。常规 UDP `block_on()` 路径会先发布 `Blocked`，本轮没有静态或动态证据将它归为首 burst 卡死的必经原因，因此没有混入未经专项回归的调度语义修改。
