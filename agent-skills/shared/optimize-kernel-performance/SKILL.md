---
name: optimize-kernel-performance
description: >-
  Ya2yOS 内核性能优化与耗时定位。用于根据 log.ans 的 QEMU/LTP/BuildStorm
  输出识别慢阶段或异常长耗时，从用户测试入口沿 syscall、fs/task/mm/net 到
  arch/driver 追踪调用路径，并使用 os/src/utils/perf.rs 的聚合计数器、tick
  计时埋点和外部 wall-clock 对比定位瓶颈、实施优化和回归验证。
---

# 内核性能优化

目标：先用可复现的时间窗口和日志证据锁定瓶颈，再做最小、可解释的优化。把
`os/src/utils/perf.rs` 当作低开销分类统计工具，不把它误当成已经实现的完整
采样 profiler 或自动生成的调用图。

## 工作流

### 1. 建立基线

1. 读取 `git status --short`，保留维护者已有改动；记录 `TARGET_ARCH`、镜像、
   initproc 入口、测试参数和 timeout。性能结果必须在同一配置下比较。
2. 先检查已有 `log.ans`，必要时把新样本写到 `/tmp/`，不要覆盖无关日志：

   ```bash
   rg -a -n "TPASS|TFAIL|TBROK|panic|ERROR|WARN|Summary|perf|BUILDSTORM|begin|end|elapsed|timeout" log.ans
   strings log.ans | tail -80
   ```

3. 记录端到端 wall-clock；宿主 `perf` 或 guest `perf_event_open` 不可用时，使用
   `/usr/bin/time`，并把 QEMU 输出与计时输出分开：

   ```bash
   /usr/bin/time -f 'elapsed_s=%e user_s=%U sys_s=%S' \
     timeout 180s make run TARGET_ARCH=riscv64 > /tmp/perf-baseline.log 2>&1
   ```

   先确认测例确实完成、输出了目标阶段的 `end`/`Summary` 或 `shutdown!`，再比较
   秒数。timeout、启动抖动、缓存冷热和残留 daemon 都不能当作性能结论。

### 2. 从日志缩小路径

1. 找到耗时区间的起止标记、重复次数和进程/架构；区分“没有输出但仍在计算”、
   “阻塞/死锁”和“测试已经失败后反复清理”。优先以 `TPASS`、`TFAIL`、
   `TBROK`、panic 和 summary 判断功能状态。
2. 从日志中的 syscall 名称、编号、模块标签或用户程序名反查源码：

   ```text
   测试/脚本 (user/src/bin 或 testcase shell)
     -> syscall 分发 (os/src/syscall/mod.rs)
     -> syscall 子模块 (fs/task/mm/net/io_mpx/...)
     -> 领域实现 (fs/task/mm/net/timer)
     -> arch/driver 或 crates/lwext4_rust/smoltcp
   ```

   用 `rg -n "符号|日志标签|sys_调用名" os/src user/src crates` 找全部实现和
   调用点；不要只凭一行日志猜测根因。对 `open/read` 等热路径同时检查 fd 查找、
   用户内存拷贝、路径解析、缓存、文件系统锁和调度唤醒。
3. 若日志只显示阶段名称，先读对应测试入口和脚本，确认它实际执行的命令、fork
   拓扑、循环次数和清理动作，再追 syscall；不要把 wrapper 的退出码或自定义
   `DEBUG` 标记当成测试结果。

### 3. 使用 `utils/perf` 采集证据

`os/src/utils/mod.rs` 导出 `perf`，实现文件是 `os/src/utils/perf.rs`。修改或
埋点前先读当前版本，因为计数器集合会随分支变化。当前工具的语义是：

| 统计 | API/输出 | 解释 |
| --- | --- | --- |
| syscall 分类 | `record_syscall(id)`，`[perf] t=... syscalls ...` | 按 Linux ABI 号聚合 read/write/open/close/stat/mm/process/futex/yield 次数 |
| EXT4 读取 | `record_ext4_read(bytes)` | 操作次数和字节数，不等于设备 I/O 时间 |
| EXT4 全局锁 | `record_ext4_lock(wait_ticks, hold_ticks)` | 等待与持有 tick；适合判断锁争用和临界区大小 |
| 文件缓存/缺页 | `record_file_cache_hit/miss/page_fault()` | 只能说明事件比例，不能单独证明加速 |
| 调度 | `record_scheduler_selection()`、`record_idle_loop()` | 选取和空闲循环的累计次数 |

计数器使用 relaxed 原子累加，不保存调用者、PID、hart 或调用栈；报告是累计值，
由 `maybe_report()` 按内核时间限频（默认约 30 秒），且只有某个采样点触发它时
才会打印。因此要把 `[perf] t=...` 对齐到 `log.ans` 的阶段标记，不能把一条快照
解释为单次调用耗时。

执行以下步骤：

1. 搜索现有接入点和被注释的候选埋点：

   ```bash
   rg -n "crate::perf|record_|maybe_report" os/src
   ```

   当前分支中 syscall、EXT4 read/lock、page cache、page fault 和 scheduler 的
   一些调用点可能是注释状态；只为本轮假设启用最少的一组，避免埋点本身改变
   调度或锁争用。
2. 需要测单一热路径时，在 `perf.rs` 增加有明确名字的 aggregate counter 和
   `record_<path>_duration(ticks)`，不要引入动态 map 或逐调用 `println!`。边界
   使用架构 tick：

   ```rust
   let begin = crate::arch::time::get_ticks();
   let result = do_hot_path();
   crate::perf::record_hot_path_duration(
       crate::arch::time::get_ticks().saturating_sub(begin),
   );
   result
   ```

   在报告中至少输出 `samples` 与 `total_ticks`，必要时增加最大值；用
   `crate::arch::time::get_clock_freq()` 将 tick 换算为微秒/毫秒。已有
   `Ext4OpGuard` 的 wait/hold 计时优先复用，不要重复包锁。
3. 先 `make` 编译，再用同一 workload 运行并采集完整日志。用 `t=...ms`、阶段
   起止时间和外部 `elapsed_s` 交叉验证；若只看到计数增长而没有耗时下降，继续
   检查阻塞、锁等待、调度切换或 I/O，而不是盲目改缓存。

### 4. 形成和验证优化

1. 将候选瓶颈写成可证伪假设，例如“父目录重复查找占用 `open14` 的大部分时间”
   或“EXT4 全局锁等待高于持有时间”；先用计数/计时验证，再改实现。
2. 优先做局部优化：复用已有 cache/父 inode、合并重复用户拷贝、缩短锁临界区、
   降低无效唤醒或避免重复路径查询。遵守任务锁序，不跨阻塞点持锁，不牺牲
   Linux 语义换取单次样本速度。
3. 每次只改变一个主要变量，重复至少两次；比较端到端 wall-clock、目标路径
   `total_ticks/samples`、计数比例和功能结果。报告中注明样本数量、架构和是否
   冷启动，不能用一次 QEMU timeout 宣称加速百分比。
4. 按风险验证：

   ```bash
   make TARGET_ARCH=riscv64
   make TARGET_ARCH=loongarch64
   make log
   make run
   ```

   只跑相关架构时明确记录另一架构未验证；行为回归仍须看 `TPASS/TFAIL/TBROK`、
   panic 和最终 summary。BuildStorm 等长测例可把长日志放在 `/tmp/`，并保留能
   复现阶段入口的命令。
5. 实验完成后删除或关闭临时埋点，确认没有逐调用日志、无意改变计数周期或把
   测试专用入口留在默认路径；用 `git diff --check` 和 `rg` 复查。

## 解释边界

- `[perf]` 是全局累计统计，不提供火焰图、函数调用图、每进程分布或硬件 cache
  miss；调用路径必须由日志、源码搜索和必要的边界埋点共同重建。
- tick 是架构相关的单调计时源；换算前读取当前 `get_clock_freq()`，不要套用另一
  架构或宿主 CPU 频率。
- 外部 `/usr/bin/time` 测的是宿主/QEMU 进程的 wall/user/sys 时间，guest
  `perf.rs` 测的是内核事件/ tick；两者不能直接相加。
- 性能优化若改变了用户可见语义、调度、锁、缓存一致性或跨架构行为，按
  `fix-bug`/`add-syscall-feature` 的验证和 `write-docs` 的记录要求收尾；至少在
  `Docs/决赛文档/开发日志.md` 留下基线、根因、改动和验证边界。
