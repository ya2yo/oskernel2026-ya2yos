# BuildStorm 共享地址空间 SMP 与远程 TLB 一致性

## 背景

`server.ans` 的 guest 日志在预构建 `axbuild` 的 `444/446` 附近长时间没有继续输出。GDB 观察到单个 `rustc` 的线程池只有一个 CPU 实际执行。`rustc` 使用 `CLONE_THREAD` 创建共享地址空间的线程，不能通过把独立进程按 PID 分布到不同 hart 来解决。

## 现象

`Process` 以 `home_hart` 固定线程组的调度归属，CFS、RR、唤醒和阻塞任务定时器扫描都读取这个字段。`sched_getaffinity` 返回在线 hart 全 mask，但 `sched_setaffinity` 只验证 mask 是否包含 `home_hart`，实际没有改变 placement。因此同一 `rustc` 的所有 `CLONE_THREAD` 线程都在同一个 hart 上执行。

## 分析

共享 `MemorySet` 在不同 hart 上运行前必须处理远端 TLB：一个线程可能在另一个 hart 的 TLB 中保留旧的 mmap、munmap、mprotect 或 COW 翻译；删除映射后如果立即释放 `FrameTracker`，远端仍可能访问已回收物理页。普通内核态 software IPI 不能直接依赖，因为当前 RISC-V kernel-trap entry 不具备可恢复的 S-mode trap 返回路径。

## 根因

旧的 `home_hart` 约束是为了掩盖没有 remote TLB shootdown 的一致性缺口。它也把任务级调度归属错误地提升为进程级属性，并造成 affinity ABI 与实际调度行为不一致。

## 修复

### RISC-V 与 LoongArch 地址空间一致性

- 新增 `os/src/mm/remote_tlb.rs`，为每个 hart 建立 sequence/ack mailbox，并用全局 update lock 串行化 shootdown 发起方，避免两个内核态 writer 互相等待对方 IPI。
- RISC-V trap decoder 增加 `SupervisorSoft -> Interrupt::Ipi`，用户态 IPI 和 idle WFI 返回都会清除 IPI、执行本地 `sfence.vma`/`fence.i` 并发布 ACK；`trap_return()` 在最终用户返回前也轮询 mailbox。
- LoongArch trap decoder 增加 `IPI -> Interrupt::Ipi`，启用 `ECFG.LIE.IPI` 和 IOCSR IPI vector，`wake_hart()` 通过 IOCSR 发送 IPI、接收端清除 `IPI_STATUS` 后轮询 mailbox。移除 `__kern_trap` 的无条件 shutdown 短路，并删去越过其 256-byte 临时帧的 user-TrapContext 写入，使 kernel IPI 能完成 pending shootdown 的本地 `invtlb`/instruction fence/ACK 后 `ertn` 返回；idle 短暂打开本地中断以接收调度唤醒。
- `MemorySet` 记录活跃 hart。用户返回时在 `MemorySet` read guard 内安装页表并发布 active bit，任务离开 processor 或 exec 替换地址空间时清除 active bit。
- `MemorySet` 的 mmap、munmap、mprotect、shm、mremap、brk、回收、fork COW 和页表写路径统一经过同步写入口。修改前保留 resident frame 的 `Arc`，远端 ACK 完成后才释放；首次不存在页的 demand fault 不做无效远端广播，已有 PTE 的 COW/权限替换仍执行 shootdown。

### 线程级 placement 与 affinity

- `TaskControlBlock` 新增 `cpu_affinity` 和 `scheduled_hart`。RISC-V 与 LoongArch 默认允许在线 hart，`CLONE_THREAD` 线程继承 mask 并从父线程 placement 的下一个允许 hart 开始选择。
- CFS、RR、唤醒 IPI、阻塞任务 timer scan、抢占、yield 和运行队列迁移均使用任务 placement。CFS 取出旧队列中的 stale entry 时会重新入队到新 hart。
- `sched_setaffinity` 解析具体 TID，检查 online/架构安全 mask，更新目标任务 placement；当前任务被排除当前 hart 时切换到允许 hart，远端运行任务收到迁移通知。`sched_getaffinity` 返回目标线程实际 mask。
- perf 汇总新增每 hart scheduler selection/idle 统计，以及 remote TLB shootdown 请求、目标 hart 数和 ACK 计数，供 BuildStorm 验证同一 rustc 是否跨 hart 执行。

### LoongArch 验证边界

初始审计曾把 LoongArch 视为没有可恢复 IPI 路径。复核 QEMU 的 IOCSR 和 `__kern_trap` 汇编后发现其原有保存/恢复/`ertn` 尾部可用，但入口被无条件 shutdown 短路；移除短路并收紧临时帧写入后，IPI 可返回原控制流，因此本轮也接入相同 mailbox 协议并解除 home-hart 限制。短时可写副本启动已看到 hart 1--11 全部在线且无 IPI/TLB panic；但没有执行能实际触发跨 hart `mmap`/COW/affinity 和 `rustc` codegen 的完整 BuildStorm，故不能把它等同于 LoongArch 性能验收。

## 涉及文件

- `os/src/mm/remote_tlb.rs`
- `os/src/mm/memory_set/handle.rs`
- `os/src/arch/riscv64/qemu/{cpu.rs,trap_interface.rs}`
- `os/src/arch/loongarch64/qemu/{cpu.rs,trap_interface.rs,asms/trap.S}`
- `os/src/trap/{mod.rs,trap_types.rs}`
- `os/src/task/{processor.rs,manager.rs,mod.rs,process/process.rs}`
- `os/src/task/scheduler/{mod.rs,cfs.rs,rr.rs}`
- `os/src/task/task/task.rs`
- `os/src/syscall/task/schedule.rs`
- `os/src/utils/perf/{scheduler.rs,report.rs}`

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make perf TARGET_ARCH=riscv64`：通过，包含 per-hart/shootdown 统计。
- `make build-arch TARGET_ARCH=loongarch64`、`make perf TARGET_ARCH=loongarch64`：通过。
- LoongArch QEMU：原镜像以只读方式启动可达 early init，但 EXT4 初始化需要写入而按预期失败；随后将镜像副本置于 `/tmp` 后，以 `-smp 12` 可写启动。移除 kernel-trap 短路后的 45 秒运行确认 hart 1--11 已进入 `rust_main`，并通过 `sigaltstack regression: PASS`、`rseq regression: PASS` 后启动 Bash/BuildStorm 脚本，无 panic、TLB 或 IPI 错误。perf 版本在 120 秒窗口内同样稳定运行到脚本启动，但未打印 perf 快照或进入 Cargo 输出。
- RISC-V 直接 QEMU 运行仍未完成：现有维护者 QEMU 实例持有 `disk.img` 写锁，第二实例收到 `Failed to get "write" lock` 后立即退出。没有终止该实例或覆盖 `disk.img`，因此本轮未声称 RISC-V QEMU 回归、LTP 或完整 BuildStorm 已通过。
