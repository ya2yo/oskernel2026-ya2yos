# BuildStorm 并行度与文件映射吞吐优化

## 背景

final-2026 镜像中的 `buildstorm::compile::run()` 会在 guest 内启动 Cargo、多个
`rustc` 子进程，并反复读取相同的 Rust 动态库和目标文件。根目录 `log.ans` 显示测试在
`Building [97/446: serde]` 后运行超过两小时，未出现 panic 或明确错误，但吞吐量不足以完成
完整构建。

## 现象

BuildStorm 通过 `nproc` 决定 Cargo worker 数量。此前 `sched_getaffinity(2)` 只返回当前进程
的 `home_hart` 位，guest 因而把八核机器识别成单核，独立的 `rustc` 子进程无法形成并行
worker pool。与此同时，文件 mmap 缺页路径只对 `MAP_SHARED` 复用全局页缓存；多个独立的
`rustc` 地址空间读取同一只读 DSO 时，每个进程都需要重新分配页帧并从 ext4 读取。

## 分析

调度器目前为了避免尚未实现的远程 TLB shootdown，将进程固定到 `home_hart`。这不影响独立
地址空间之间的并行执行，但把内部实现细节直接暴露给 `sched_getaffinity` 会改变 Linux 用户
态看到的 CPU 拓扑。Cargo/rustc 使用该 syscall 的结果设置并发度，因此这是性能退化的首要
根因。

文件页缓存的页帧由 `FrameTracker::alloc()` 清零，缓存命中后可安全地映射为只读页。对
`MAP_PRIVATE` 的可写映射，映射时必须清除 PTE 写权限并设置 COW，第一次写入再通过正常的
write-protect 路径分裂页帧；否则会把私有映射的写入传播给其他进程。

## 根因

1. `sched_getaffinity` 返回单个 `home_hart`，使 guest 中 `nproc=1`，Cargo 退化为串行构建。
2. 文件页缓存的复用条件过窄，未覆盖干净的 `MAP_PRIVATE` 文件映射。
3. 缺页读取先创建临时 `Vec<u8>`，再复制到新页；新页分配后还进行逐字节清零检查，增加了
   每次 mmap fault 的无效成本。

## 修复

- `os/src/syscall/task/schedule.rs` 继续校验目标 PID/TID，但返回 `HART_NUM` 个在线 CPU 的
  mask。进程内部仍固定 `home_hart`，只把 Linux-visible topology 恢复为 SMP 配置，独立子
  进程可以分别运行在不同 hart。
- `os/src/mm/page_fault_handler.rs` 将文件页缓存命中扩展到所有干净文件映射，包括
  `MAP_PRIVATE`；架构页表 helper 对可写私有映射设置 RISC-V/LoongArch COW。私有 writable
  fault 不直接复用全局缓存，`MAP_SHARED` writable fault 保持原有共享语义。
- 文件缺页直接把 `inode.read_at()` 写入新页帧，移除中间缓冲区；同时依赖帧分配器的清零保
  证，删除重复的逐字节零检查（`os/src/fs/page_cache.rs`、`os/src/mm/map_area.rs`）。
- 曾尝试在 `Ext4BlockWrapper::new()` 挂载后把 lwext4 bcache 从 16 项运行时重建为 4096
  项，但该时机可能清空 journal 仍持有的缓存引用，实测会使 `fs::init` 卡住。因此没有保留
  该不安全改动；如需扩大 bcache，应在具备目标 C 工具链时通过 lwext4 编译期配置完成。

## 涉及文件

- `os/src/syscall/task/schedule.rs`
- `os/src/mm/page_fault_handler.rs`
- `os/src/fs/page_cache.rs`
- `os/src/mm/map_area.rs`

`user/src/bin/initproc.rs` 的定向 BuildStorm 入口是维护者已有工作区改动，本问题没有改变
其正式测试脚本语义。

## 验证

- `make`：RISC-V 与 LoongArch64 release 用户程序、内核均成功构建；仅有未改动 `smoltcp`
  的既有 warning。
- `timeout 180s make run`（RISC-V，final-2026 镜像）：完成文件系统、网络和八个 HART
  启动，进入 `tg-xtask` 的 Cargo 预构建，日志出现多个 crate 的并行编译队列；未出现
  panic、错误或文件系统初始化卡死。
- 没有在本轮完成 446 个单元的完整 BuildStorm，也没有新旧版本在相同干净 guest 上的
  `elapsed_s` A/B 数据，不能据此宣称具体加速百分比或正式评分通过。
