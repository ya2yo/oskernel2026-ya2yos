# BuildStorm P0 分解计数与阶段基线

## 背景

《优化方案》P0 要求先把 EXT4 resource lock、block request 和写回路径拆成可比较的聚合数据，再决定是否进入锁临界区或请求合并优化。本轮只补 instrumentation，不改变 EXT4 锁序、bcache 所有权、设备排队策略或文件可见性语义。

## 修改

- 将 `BlockRequestPerf` 放入 `os/src/utils/perf/block.rs`，`disk.rs` 仅保留设备请求生命周期和实际 I/O。
- 为 Rust block request 增加读/写/flush 分类、请求字节、队列等待、设备 service、错误、最大请求大小和 512 字节对齐。连续性以已经取得 `Disk::submission` 的真实设备顺序计算；flush 会截断相邻请求链。
- 为 lwext4 bcache 状态等待增加 tick 聚合，单独输出 `ext4_bcache_completion_wait`，不与 Rust 设备 submission queue wait 混算。
- 为每个任务维护 perf 专用的 resource-lock 类别引用计数。`ext4_lock_io` 将该稳定上下文与 read/write/flush 请求关联，分别记录提交、submission queue wait 和 device service；bcache completion wait 还拆分为仍持锁和已解锁两部分。
- 保留现有 resource lock class 的 acquire、contended、queued、wait、hold、max hold 统计，并继续输出 sparse/dense/metadata/writeback 阶段统计。为避免在 C 侧增加 telemetry，当前不统计
  journal commit，也不把 block flush 伪作 journal commit。
- 在 `initproc` 的 CAgent、正式 BuildStorm 和嵌套 QEMU 前后增加一次性 `BUILDSTORM_PHASE` marker，便于按阶段截取 perf interval。
- `task::idle_hart_snapshot()` 改为显式 crate 内包装函数，避免 `report.rs` 经私有模块重导出时无法可靠跳转到定义。

## 统计口径

`interval_ext4_block_device` 中新增请求形态字段按报告周期取 delta；`ext4_block_device` 中为累计值。请求顺序性按取得串行设备提交权后的上一个请求结束偏移判断，而不是按 Hart 记录，因此任务迁移和同 Hart 交错不会造成伪连续性；它只作诊断统计，不驱动请求合并。

`interval_ext4_lock_io` / `ext4_lock_io` 的 `lock_class` 是请求提交时当前任务仍持有的 lwext4 资源锁类别，行中的 `queue_wait_*` 是提交前等待设备 token 的时间，`service_*` 是提交后同步设备服务时间。一个请求同时持有嵌套类别时会在每个类别行中出现，这些行不是可相加分区。`ext4_bcache_completion_wait` 的 `lock_held_*` 与 `unlocked_*` 分别对应回调开始时仍有/没有 resource lock；该回调不携带 read/write 方向，因而不把它伪造为某类块请求的 service。

`Journal` resource-lock 行仍可用于查看持有 journal 锁时的 block I/O 与等待；它不等同于 JBD
transaction commit 数。后续如需该计数，应优先寻找 Rust 可见且不增加 C 侧统计代码的事务边界。

## 涉及文件

- `os/src/utils/perf/block.rs`
- `os/src/utils/perf/fs.rs`
- `os/src/utils/perf/mod.rs`
- `os/src/utils/perf/report.rs`
- `os/src/drivers/disk.rs`
- `os/src/fs/ext4_lw/mod.rs`
- `os/src/fs/ext4_lw/sb.rs`
- `os/src/task/mod.rs`
- `os/src/task/task/task.rs`
- `user/src/bin/initproc.rs`
- `crates/lwext4_rust/c/lwext4/include/ext4_bcache.h`
- `crates/lwext4_rust/c/lwext4/src/ext4_bcache.c`
- `crates/lwext4_rust/src/perf.rs`

## 验证

`make perf TARGET_ARCH=riscv64`、`make perf TARGET_ARCH=loongarch64`、`make build-arch TARGET_ARCH=riscv64` 和 `make build-arch TARGET_ARCH=loongarch64` 均通过。构建只报告既有 `.cargo/config` 弃用、`smoltcp` 未使用项和 `lwext4_rust` 的未构造枚举警告。

对两种架构的 lwext4 `no-perf` archive 使用交叉 `nm -g --defined-only` 检查，均未发现
`ext4_bcache_perf_*` 导出符号；对应 `perf` archive 则导出 `enable`、`snapshot` 和 I/O/writeback
记录函数。这证明普通构建没有链接 C 侧 telemetry 实现。

未运行 QEMU、LTP、CAgent 或完整 BuildStorm：工作区未跟踪的 `disk.img` 是指向正式 LoongArch 镜像的符号链接，而根目录 `make run` 会先删除并重建该链接。新增统计仍须在固定源码 commit、架构、Hart 数、镜像、initproc 测例和 perf 开关下取得完整阶段日志后再比较；本轮不报告性能收益。
