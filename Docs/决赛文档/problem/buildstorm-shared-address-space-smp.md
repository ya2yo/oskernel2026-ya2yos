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

## 2026-08-06：shootdown 等待环修复

### 新现场

新的 RISC-V `server.ans` 仍只到 `OS COMP TEST GROUP START buildstorm`。最新
`client.ans` 表明此前的 address-space writer 已在 `MemorySet::mprotect()` 中：hart 7 持有
`UPDATE_LOCK` 和 `MemorySet` write lock，在 `remote_tlb::shootdown()` 等待 hart 0 的 ACK；hart
0 则在 `trap_return() -> rseq_prepare_user_return() -> copy_to_user() -> MemorySet::get_ref()`
等待同一把 read lock。hart 0 仍在该 `MemorySet` 的 active mask 内，不能让发起方根据
live mask 放弃等待。

此前一份 GDB 现场还有相邻变体：hart 7 对匿名 `mmap(PROT_NONE)` 做无条件 shootdown，hart 0
已清除 active bit 后在 `lock_updates()` 自旋。该映射只新增 lazy VMA、没有改写现有 PTE，却触发
了不必要的全 hart IPI。

### 根因

mailbox 协议原先只在 IPI trap、idle 返回和最终 `trap_return()` 处轮询。只要一个 active hart
在关中断的内核路径中等待 `UPDATE_LOCK` 或 `MemorySet` read lock，就可能在到达这些轮询点之前
成为 shootdown target。于是 writer 等 ACK、target 等 writer 所持的锁，形成死锁。单次
`active_harts` 快照还会让 writer 等待已经脱离该地址空间的 hart。

### 修复

- `lock_updates()` 改用 `try_lock()` 循环；每次竞争失败先调用 `remote_tlb::poll()`，使等待
  update lock 的 target 可以直接完成本地 TLB/指令缓存失效和 ACK。
- `MemorySet::get_ref()` 同样改为 `try_read()` 加 mailbox poll 循环，`activate_for_user()` 复用
  此入口。因而 rseq、用户内存复制、权限检查或最终地址空间激活在等待 write lock 时也可 ACK。
- `shootdown()` 接收 `active_harts` 原子对象而非一次性值，在每个 ACK 等待循环重新读取目标 bit；
  目标已清除 bit 时可安全退出等待，因为它无法在 writer 释放 write lock 前重新发布用户态 active bit，
  后续 `activate_for_user()` 会做本地页表激活和刷新。
- 非 `MAP_FIXED` 的 `mmap`（包括 `MAP_FIXED_NOREPLACE`）只创建 lazy VMA，不会删除或替换 PTE，
  改走不保留全量 frame、也不发 remote shootdown 的专用写路径。`MAP_FIXED`、`munmap`、
  `mprotect`、COW 和所有可能回收旧 frame 的路径仍使用完整 ACK 协议。

该协议位于共同的 `remote_tlb`/`MemorySet` 层；RISC-V 的 SBI software IPI 和 LoongArch 的 IOCSR
IPI 接收路径都继续使用同一个 mailbox，不需要为本修复分叉架构语义。

### 验证更新

- `rustfmt --edition 2021 os/src/mm/remote_tlb.rs os/src/mm/memory_set/handle.rs` 与
  `git diff --check`：通过。
- `make perf TARGET_ARCH=riscv64`：通过，生成包含本修复的 `kernel-rv`。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- 最新 GDB 现场是在加入 `UPDATE_LOCK`/live-mask 修复后、加入 `get_ref()` 轮询前采集，直接证明
  第二个读锁等待环；因此不能把它当成最终运行期通过。受限环境的本地 QEMU `-snapshot` 不能在只读
  `/var/tmp` 创建临时 overlay，且没有覆盖维护者镜像；完整 RISC-V BuildStorm、LTP 以及 LoongArch
  跨 hart 长时回归仍待用最新镜像执行。
- 随后维护者运行的最新 `kernel-rv` 已越过旧的启动卡点，在 `t=188070ms` 到达
  `443/446: axbuild, axvmconfig`。该快照的 `remote_tlb` 为 `shootdowns=21578`、
  `target_harts=2654`、`acknowledgements=2654`，没有未确认目标；所有 8 个 hart 都有 scheduler
  selection，且累计 `remote_enqueues=2831`。日志尚无 `BUILDSTORM_COMPILE`、测试组 END 或
  `shutdown`，因此这只验收两个已定位的 shootdown 等待环被越过，不作为完整 BuildStorm 通过或
  axbuild 性能结论。当前 `client.ans` 只含 GDB 连接记录，若再次长时间无输出，需要在 live QEMU
  上采集各 hart backtrace 才能定位下一处阻塞。

## 2026-08-07：fork 固定栈复制的 MemorySet 锁反转

### 新现场

`server.ans` 停在 tg-xtask 预构建的 `ax-cpumask` 与 `ax-memory-addr`。对应
`client.ans` 中 5 个 hart 已进入 idle，另外 3 个 hart 的调用栈为：

- hart 0 在 `clone_process() -> MemorySet::lazy_clone_area() -> with_mut()` 中等待
  `UPDATE_LOCK`；
- hart 2 在 `handle_page_fault()` 中已经取得 `UPDATE_LOCK`，随后等待
  `MemorySet::inner` write lock；
- hart 1 在另一个 `handle_page_fault()` 中等待 `UPDATE_LOCK`。

`clone_process()` 在进入 `lazy_clone_area()` 前通过 `parent_memory_set_arc.get_ref()`
持有父地址空间 read guard。因而 hart 0 的实际锁链是“父 `MemorySet` read ->
`UPDATE_LOCK`”，hart 2 则是协议规定的“`UPDATE_LOCK` -> 父 `MemorySet` write”，
两者构成 AB-BA 循环。

### 根因

远程 TLB 协议引入 `UPDATE_LOCK -> MemorySet::inner` 顺序后，旧的跨地址空间栈复制接口
仍要求调用者把源 `MemorySetInner` guard 传入目标地址空间写操作。该接口隐式要求同时持有
源、目标两侧锁，违反了新协议；`RemoteTlbMutex` 的 mailbox 轮询只能处理 ACK，不能解除
这种真正的锁依赖环。

### 修复

- `MemorySet::lazy_clone_area()` 改为接收源 `MemorySet`，先在源 read guard 内克隆固定栈
  已驻留页的 `Arc<FrameTracker>`，随即释放源 guard；然后才通过 `with_mut()` 获取
  `UPDATE_LOCK` 和目标 write lock。
- `MemorySetInner::lazy_clone_area()` 只消费上述 frame 快照。`Arc` 保证源物理页在复制完成前
  不会回收，同时避免为最多 8 MiB 的固定栈再分配一份中间字节缓冲。
- 在 `task` 总入口和 `mm::memory_set` 模块顶部声明完整顺序：task/process/resource-slot 层之后，
  MM 写路径固定为 `UPDATE_LOCK -> 单个 MemorySet -> MM 子锁`；禁止持有任意
  `MemorySet` guard 时获取 `UPDATE_LOCK`，跨地址空间操作必须先快照再换锁。

### 涉及文件

- `os/src/task/mod.rs`
- `os/src/task/task/task.rs`
- `os/src/mm/memory_set/mod.rs`
- `os/src/mm/memory_set/handle.rs`
- `os/src/mm/memory_set/area_ops.rs`

### 验证更新

- `git diff --check`：通过。
- `make TARGET_ARCH=riscv64`：默认 `all` 目标完成 RISC-V 与 LoongArch64 release 构建。
- RISC-V QEMU 使用现有 BuildStorm-only initproc 运行：从旧日志固定的 `440/446`
  两个 crate 继续完成 `axvm-types`、`axvmconfig` 并推进到 `444/446: axbuild`，之后进入
  `BUILDSTORM_BEGIN mode=multi`，期间没有 panic 或同类锁等待。
- tg-xtask 预构建后段写 `libaxbuild*.rmeta` 时遇到独立的 ext4 `EIO`，因此本轮没有报告
  完整 BuildStorm 通过；确认正式阶段开始后人工结束 QEMU。完整端到端与 LTP 未执行。
- `cargo fmt --all -- --check` 仍被本次范围外的既有格式差异阻断，没有格式化无关文件。
