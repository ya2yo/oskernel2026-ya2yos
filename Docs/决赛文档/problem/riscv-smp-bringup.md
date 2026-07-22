# RISC-V 双 hart SMP bring-up

## 背景

Ya2yOS 原先按单 hart 运行：QEMU 启动参数为一个 CPU，`hart_id()` 恒为 0，若干全局状态借助未启用 SMP 原子支持的 `kspin::SpinNoIrq` 或 `static mut` 访问。维护者要求扩展为双核运行，并保持 LoongArch64 的现有单核启动可用。

本次实现的目标是 RISC-V QEMU 上两个 hart 同时参与调度。它不是完整 Linux SMP：暂未实现 IPI 唤醒、远程 TLB shootdown、per-CPU ready queue 或 work stealing。

## 现象

开启 QEMU 的第二个 hart 后，bring-up 期间先后出现：

- 任意 hart 可能成为 OpenSBI boot hart，旧的“读后写”首核判定会让两个 hart 同时执行全局初始化。
- 调度器、PID/TID 映射和资源槽将 `try_lock()` 竞争直接视为 panic；多 hart 下正常竞争会立即触发。
- 进程回收把其他核临时持有的 `Arc<Process>` 当成引用泄漏；`waitpid` 也可能在 group exit code 发布前观察到 task list 已空。
- `lwext4` 的全局 buffer cache 并非并发安全，双 hart 文件操作会破坏其红黑树并在 `ext4_bcache.c` page fault。
- 依赖未启用 SMP feature 的 `kspin::SpinNoIrq` 不含实际原子锁；共享 `BTreeMap` 因而在 `iperf` 压力下损坏。
- 异步定时轮 `TIMER_RUNTIME` 是未保护的 `static mut BTreeMap`，两个 hart 同时检查、注册或取消 timer future 时会破坏状态或丢失超时唤醒。

## 分析

SBI `hart_start` 需要内核物理入口，而 `_start` 是高半内核虚拟地址，因此启动地址必须减去 `KERNEL_ADDR_OFFSET`。启动路径还必须区分全局初始化与每 hart 本地初始化，并以 acquire/release 发布状态，不能假定 hart 0 一定是 BSP。

未实现远程 TLB shootdown 时，两个线程不能在不同 hart 同时使用同一个 `MemorySet`；否则 COW、`munmap` 或权限变更后另一核仍可能持有过期 TLB。因此按进程而不是按线程固定 home hart：同一线程组共享地址空间，只在同一 hart 运行；`fork` 后的新进程拥有独立地址空间，可被分配到另一 hart。

## 根因

单核实现把“不会同时发生”的假设编码在启动判定、无锁全局可变状态、竞争即 panic 和不完整的锁实现中。切换为两个真正并发执行的 hart 后，这些假设均不成立。文件系统和网络的 timer future 还依赖共享容器，因此会在压力路径中放大为内存破坏或永久等待。

## 修复

- RISC-V QEMU 改为 `SMP := 2`，`hart_id()` 读取真实 `tp`；BSP 用 SBI HSM `hart_start` 启动 AP。
- `BOOT_STATE` 使用 `compare_exchange`、Acquire/Release 状态机串行化全局初始化；AP 等待上线后只进行自身 trap、页表和 timer 初始化。
- `Process` 增加 `home_hart`。RISC-V 按 pid 轮转分配，ready queue 只向 owner hart 取任务；LoongArch64 仍固定 hart 0。
- 将共享任务、进程、内存资源的 `try_lock()`/`try_read()`/`try_write()` 改为实际等待锁；修复多核下进程回收和 group exit 发布顺序。
- 为所有 lwext4 C 接口调用增加一个实际生效的 `spin::Mutex`，并避开符号链接递归等可能二次取锁的路径。
- 共享 poll IRQ 表、waker 状态、pselect waiter 集合和异步定时轮均改由 `spin::Mutex` 保护；定时轮锁前同时禁止本地中断重入。

## 涉及文件

- `scripts/riscv64.mk`
- `os/src/config.rs`
- `os/src/main.rs`
- `os/src/arch/riscv64/qemu/cpu.rs`
- `os/src/trap/mod.rs`
- `os/src/task/{manager.rs,processor.rs,mod.rs}`
- `os/src/task/process/process.rs`
- `os/src/task/task/task.rs`
- `os/src/task/future/{mod.rs,poll.rs,time.rs}`
- `os/src/mm/memory_set/handle.rs`
- `os/src/utils/resource_slot.rs`
- `os/src/fs/ext4_lw/{mod.rs,sb.rs,inode.rs}`
- `os/src/signal/timer.rs`

## 验证

- `make build-arch TARGET_ARCH=riscv64` 通过。
- `timeout 120s make run TARGET_ARCH=riscv64`：OpenSBI 报告 `Platform HART Count : 2`，日志包含 `boot secondary harts...complete.` 与 hart 1 启动消息；`iperf-musl`、`iperf-glibc` 的 basic、parallel、reverse UDP/TCP 用例均 success，最终 `shutdown!`，无 `panic`、`TFAIL` 或 `TBROK`。
- `make build-arch TARGET_ARCH=loongarch64` 通过。
- `timeout 60s make run TARGET_ARCH=loongarch64`：该架构保持 `SMP := 1`；`iperf-musl`、`iperf-glibc` 均完成并 `shutdown!`，无内核 panic。

## 限制与后续

- 本实现不提供 IPI、远程 TLB shootdown acknowledgement 或活跃 hart mask，因此同一 `Process` 不跨 hart 运行。
- ready queue 仍为一个全局队列，扫描非 owner 任务会产生锁竞争；ext4 采用全局操作锁，I/O 不会线性扩展。
- `spin::Mutex` 仅解决当前已验证的共享数据竞态。后续应提供 IRQ-safe SMP lock、per-CPU queue/work stealing 和跨核唤醒 IPI，并在解除进程亲和性前实现完整 TLB shootdown。
