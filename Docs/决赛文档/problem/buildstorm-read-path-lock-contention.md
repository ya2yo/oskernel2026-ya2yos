# BuildStorm 普通 read 路径与 EXT4 全局锁争用

## 背景

`buildstorm::compile::run()` 在 `cargo build -p tg-xtask` 预构建阶段长时间停留，用户态
没有报错，但单个 crate 的编译耗时明显过长。此前的调度器和私有文件页缓存优化已经让
guest 暴露八个 CPU，但仍需要确认普通文件读取是否重复执行了不必要的 EXT4 工作。

## 现象

- `log.ans` 和本轮独占 RISC-V 运行都进入 `pre-build tg-xtask`，180 秒窗口只显示
  `Building 0/446`，没有 `panic`、`TFAIL` 或文件系统错误。
- 宿主 `/usr/bin/time -v` 的 60 秒样本为 user 59.82 s、system 14.74 s、CPU 约 124%，
  voluntary context switch 479,964 次。
- `strace -f -c` 的 30 秒样本中 `futex` 占 79.46%，`wait4` 占 16.09%，`pread64` 仅
  0.18%。`strace` 会改变绝对耗时，因此这些数据只用于确认调用结构，不用于 A/B 百分比。
- 主机没有可用的 `perf`，guest 的 `perf_event_open` 仍是未实现桩，故没有伪造 perf 结果。

## 分析

`Ext4Inode::read_at()` 原先在每次读取中持有 `EXT4_OP_LOCK`，调用 path-based
`ext4_fopen`、`ext4_fseek` 和 `ext4_fread`。`file_seek()` 还会检查并可能建立整文件写回
缓存；这对 Rustc 大量一次性读取的小文件会产生重复路径查找、缓存表操作和数据复制。

`OSFile::read()` 还会先调用一次 `inode.size()` 判断 EOF，再按用户缓冲区的每个页片段
调用 `read_at()`。跨页的一个 read syscall 因此会重复获取 EXT4 全局锁。lwext4 的挂载块
缓存仍是共享且非 SMP-safe，不能简单删除全局锁来换取并行度。

## 根因

已确认的高频额外工作是普通只读 read 的 `size + seek + cache probe` 组合，以及跨页用户
缓冲区导致的重复锁进入。全局 EXT4 锁本身仍是 lwext4 当前安全边界，不能在本问题中直接
放宽为无锁并发。

## 修复

- `crates/lwext4_rust/src/file.rs`
  - 增加 `file_open_read_only()`，只读打开不触发整文件写回缓存准备和稀疏布局探测。
  - 增加 `file_read_at()`，设置底层偏移后直接执行一次 `ext4_fread`，跳过额外 `fseek`。
  - 增加 `read_cached_at()`；已有脏的 whole-file cache 时仍优先读取缓存，避免写后读返回
    旧磁盘数据。
- `os/src/fs/ext4_lw/inode.rs`
  - 普通 `read_at()` 先尝试缓存，冷只读路径改为 `file_open_read_only + file_read_at`。
  - 空缓冲直接返回；`read_all()` 和 `size()` 的只读打开也不再主动建立写回缓存。
- `os/src/fs/files/os_file.rs`
  - 移除每次 `read()` 前的冗余 `inode.size()` 探测，由 `read_at()` 的 0 字节结果表达 EOF。
  - 用户缓冲跨页且总长不超过 64 KiB 时合并到一次临时连续缓冲，再只进入一次 inode read；
    单页路径保持零拷贝，更大的请求继续流式逐页处理，避免用户长度触发无界内核分配。
- `os/src/task/manager.rs`、`os/src/task/processor.rs`、`os/src/timer/mod.rs`
  - 保留此前调度优化：定时器维护扫描按 hart 快照任务，并限制为每 10 ms 一次，避免在
    编译高切换率下反复争用全局任务表和 timer 锁。

## 验证

- `rustfmt --check`、`git diff --check`：通过。
- `make TARGET_ARCH=riscv64`：通过（同时完成仓库默认的 LoongArch64 子构建）。
- `make TARGET_ARCH=loongarch64`：通过；后续增量构建再次完成 RISC-V、LoongArch64。
- 独占宿主 QEMU RISC-V 180 秒：成功启动到 `buildstorm-compile` 和
  `pre-build tg-xtask`，无 panic/错误，但仍未完成 446 个 crate，不能据此给出加速比例。
- 独占宿主 QEMU RISC-V 70 秒烟测：进入同一预构建阶段，无 panic、`TFAIL` 或文件系统错误。
- 完整 BuildStorm、严格同镜像 A/B 耗时和正式评分尚未完成；当前结论是减少了可证明的
  额外读取/锁操作，不宣称已经解决全部编译吞吐问题。

## 后续观测

为避免继续凭单一假设修改文件系统，内核新增 `os/src/perf.rs` 聚合计数器，并在系统调用、
EXT4 锁、文件页缓存、文件 mmap 缺页和调度器路径中记录低开销统计。汇总最多每 30 秒输出
三行 `[perf]`，不输出逐条热路径日志，也不改变 lwext4 的全局 SMP 安全锁。

独占 RISC-V QEMU 的 90 秒样本（`/tmp/buildstorm-perf-90.ans`）得到：

- `t=63606ms`：累计 syscall `554295`，其中 futex `133434`；
- 调度器 `selections=6317628`，idle loops `42750`；
- EXT4 reads `11175`、约 `111822854` bytes，锁等待 `472198` tick，锁持有 `263219485` tick。

该样本没有 panic、`TFAIL`、`TBROK` 或文件系统错误，但未完成 446 crate。调度选取次数远高于
syscall 和 EXT4 操作次数，说明下一轮应先拆分真实上下文切换与调度循环/ready queue 的
重复选取，再决定是否继续扩大文件系统并行度；当前不能把 EXT4 锁认定为唯一根因，也不能
据此宣称完整编译已提速或正式评分通过。
