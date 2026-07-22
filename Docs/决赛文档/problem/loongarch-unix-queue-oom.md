# LoongArch AF_UNIX 无界队列 OOM 与 RISC-V 2GiB 启动

## 背景

`loongarch.ans` 在完整测试序列的 `cyclictest-glibc` 压力阶段停止：后台
`hackbench` 以 400 个 task 和 AF_UNIX `socketpair()` 持续交换 100-byte 消息。维护者同时要求
将 LoongArch QEMU 内存上限调整为 2GiB，并确认分段内存能否合并；随后要求 RISC-V QEMU
也调整为 2GiB。

## 现象

原始日志在 `cyclictest-glibc` 的 `STRESS_P8` 输出后 panic：

```text
[kernel] Panicked at src/mm/heap_allocator.rs:18 Heap allocation error,
layout = Layout { size: 917504, align: 8 (1 << 3) }
```

启动日志显示旧的 1GiB RAM 已按两段加入 CMA：

```text
from: 0x9000000003a90000
to:   0x9000000010000000
from: 0x9000000080000000
to:   0x90000000b0000000
```

因此不能仅凭 panic 断定第二段 RAM 未被使用。

## 分析

QEMU 9.2.1 对 `-m 2G` 生成的设备树含两个 memory node：

| 物理范围 | 大小 | 说明 |
| --- | --- | --- |
| `0x00000000..0x10000000` | 256MiB | 低端 RAM，内核镜像装载位置 |
| `0x80000000..0xf0000000` | 1792MiB | PCI/MMIO hole 之后的高端 RAM |

中间 `0x10000000..0x80000000` 不是 RAM，不能把它伪造成一块连续物理内存。现有
`init_cma_heap()` 对每个范围调用同一个 `CMA_ALLOCATOR.add_to_heap()`，所以对页帧分配器而言
两段已经被逻辑合并；该路径不应改回把空洞加入 CMA 的连续区间实现。

GDB 在 `handle_alloc_error` 断点捕获的调用链为：

```text
alloc::raw_vec::RawVec<UnixMessage>::grow_one
VecDeque<UnixMessage>::push_back
os::net::unix::UnixSocket::send
os::fs::files::net::Socket::write
os::syscall::io_mpx::file::sys_write
```

`UnixSocket::send()` 原来无条件将消息压入 peer 的 `recv_queue`。`UNIX_BUF_SIZE = 64KiB`
只用于 `getsockopt()` 返回值，未限制队列实际占用；`hackbench` 的生产速度高于接收速度时，
`VecDeque` 会无限扩容。panic 报告的 896KiB 正是该 `VecDeque` 扩容请求。

全局内核堆 `HEAP_ALLOCATOR` 使用独立的 48MiB 静态 BSS，不属于 CMA。因此即使 CMA 尚有
可用页，AF_UNIX 元数据的无界增长仍会耗尽或碎片化全局堆并 panic。

RISC-V QEMU `virt` 在 `-m 2G` 时的 DTB 是一段连续 RAM：
`[0x80000000, 0x100000000)`。但 RISC-V `entry.asm` 的 bootstrap 页表只有一个 1GiB leaf，
在切换 `satp` 前仅直接映射 `[0x80000000, 0xc0000000)`。`buddy_system_allocator::add_to_heap()`
会在被管理的空闲块中写入链表节点，若启动早期直接把第二个 GiB 加进 CMA，会访问未映射的
`KERNEL_ADDR_OFFSET + PA`，导致内核停在 `init_cma()`。此外，逐页建立 2GiB 直接映射会产生
大量页表操作；RISC-V Sv39 支持 2MiB 和 1GiB leaf PTE，适合仅用于不可变的内核物理直接映射。

## 根因

1. AF_UNIX 接收队列没有执行 socket buffer 上限，也没有满队列时的写端阻塞/唤醒机制。
2. LoongArch 的 QEMU RAM 被硬件地址洞分段；此前 1GiB 配置正确避开了该洞，但容量不足以覆盖
   当前高并发压力的余量。
3. 48MiB 静态内核堆对于 2GiB 配置下的正常任务、页表和文件系统元数据余量过小。
4. RISC-V 不能只把 QEMU `-m` 和 `PHYSICAL_MEMORY_SIZE` 改为 2GiB：CMA 的自引用元数据必须在
   bootstrap 直接映射可见范围内初始化，第二个 GiB 须延后加入。

## 修复

### 2GiB 分段物理 RAM

- `scripts/loongarch64.mk` 将 QEMU `-m` 从 `1G` 改为 `2G`。
- `PHYSICAL_MEMORY_RANGES` 更新为
  `[(0x0000_0000, 0x1000_0000), (0x8000_0000, 0x7000_0000)]`，总计 2GiB。
- 保持两段范围通过同一个 CMA `LockedHeap` 逻辑合并，不分配 PCI/MMIO hole。
- 将 LoongArch 静态内核堆从 48MiB 提升到 128MiB；链接后的内核仍完全位于低端 256MiB RAM。

### AF_UNIX 有界接收队列

- 将 `recv_queue` 封装为同时保存消息、已排队字节数和 read-shutdown 状态的结构。
- 发送前检查剩余空间。`SOCK_STREAM` 可写入可用的前缀并返回实际字节数；`SOCK_DGRAM` 和
  `SOCK_SEQPACKET` 保持消息原子性，超过 64KiB 返回 `EMSGSIZE`。
- 队列满时，阻塞 socket 的 `write/send` 经 `poll_io()` 等待目标队列的 `write_poll`；
  `O_NONBLOCK` 和 `MSG_DONTWAIT` 返回 `EAGAIN`。
- 接收、部分 stream 接收、`SHUT_RD` 丢弃队列和 socket drop 都唤醒等待的写端。发送等待器绑定
  到本次发送实际选定的目标队列，因此同样覆盖未 `connect()` 的 datagram `sendto()`。
- `recv()` 在同一把队列锁内完成取消息、用户缓冲拷贝、字节计数更新和 stream 剩余数据回插，
  避免并发 `SHUT_RD` 清空队列后发生 `queued_bytes` 下溢。

### RISC-V 连续 2GiB 与两阶段 CMA

- `scripts/riscv64.mk` 和 RISC-V 内存布局同步到 QEMU `virt` 的
  `[0x80000000, 0x100000000)` 连续 2GiB RAM。
- 启动早期 CMA 只加入内核镜像之后到 `0xc0000000` 的可见范围；建立并激活完整内核页表后，
  再将 `[0xc0000000, 0x100000000)` 通过同一个 `CMA_ALLOCATOR.add_to_heap()` 纳入。因此最终
  仍是一个总计 2GiB 的连续物理页分配池，而不是两套 allocator。
- RISC-V 的内核物理直接映射使用最大可对齐的 Sv39 1GiB、2MiB leaf PTE；`translate()` 同时
  识别 4KiB、2MiB 和 1GiB leaf。用户页、ELF、COW 和普通 mmap 仍走原有的 4KiB 映射路径。
- RISC-V 静态 `KERNEL_HEAP_SIZE` 仍为 48MiB。增加物理内存会增加 CMA 可用页，但不会自动扩大
  全局 BSS 堆；本问题中的无界全局堆增长由 AF_UNIX 队列上限单独消除。

## 涉及文件

| 文件 | 修改 |
| --- | --- |
| `scripts/loongarch64.mk` | LoongArch QEMU 内存上限改为 2GiB |
| `os/src/arch/loongarch64/qemu/memory_layout.rs` | 更新真实分段 RAM 表，总物理内存和静态内核堆大小 |
| `scripts/riscv64.mk` | RISC-V QEMU 内存上限改为 2GiB |
| `os/src/arch/riscv64/qemu/memory_layout.rs` | RISC-V 连续 2GiB RAM 与 1GiB bootstrap 映射边界 |
| `os/src/arch/riscv64/qemu/page_table.rs`、`os/src/mm/map_area.rs` | 内核物理直接映射的 Sv39 2MiB/1GiB leaf PTE |
| `os/src/mm/frame_alloc/buddy_cma.rs`、`os/src/mm/mod.rs` | RISC-V CMA 启动早期/页表激活后的两阶段初始化 |
| `os/src/net/unix.rs` | AF_UNIX 接收队列字节计费、满队列背压和可写 waker |

## 验证

已执行：

```text
make TARGET_ARCH=loongarch64
timeout 480s make run TARGET_ARCH=loongarch64 \
  > /tmp/loongarch-2g-unix-queue.log 2>&1
git diff --check
```

LoongArch64 构建通过，仅保留已有的 `smoltcp` vendor warnings。QEMU 启动日志确认：

```text
from: 0x9000000008a91000
to:   0x9000000010000000
from: 0x9000000080000000
to:   0x90000000f0000000
```

该日志中 `cyclictest-musl` 和 `cyclictest-glibc` 的 `NO_STRESS_P1`、`NO_STRESS_P8`、
`STRESS_P1`、`STRESS_P8` 均为 success，两个 `kill hackbench` 均 success，未出现
`Heap allocation error`、`CMA OOM` 或 panic。确认目标路径后停止了无关的后续长时 libctest/LTP
执行，因此未将完整全量回归视为本次验证结果。

`make TARGET_ARCH=riscv64` 和 `make TARGET_ARCH=loongarch64` 均通过，只有既有的 `smoltcp`
vendor warnings。RISC-V 2GiB QEMU 启动日志确认 CMA 分两阶段初始化：

```text
from: 0xffffffc0833d3000
to:   0xffffffc0c0000000
mm:kernel pagetable activated
init_cma_late:
from: 0xffffffc0c0000000
to:   0xffffffc100000000
mm:remap_test complete, mm_init is finished
task::add_initproc
```

该运行完成 busybox/Lua 的 musl、glibc 两组，完成 iperf-musl 六项并在 iperf-glibc 的前四项
成功后到达外部 60 秒限制；日志没有 `Heap allocation error`、`CMA OOM` 或 kernel panic。本轮最后
一次 LoongArch 2GiB 启动完成 busybox、Lua、iperf 的 musl/glibc 全部基础项，未出现上述错误，
但 QEMU 在进入 cyclictest 前结束；因此完整 cyclictest/hackbench 结论仍以先前的 480 秒压力日志
为准，而不是把该次短运行记作完整压力回归。
