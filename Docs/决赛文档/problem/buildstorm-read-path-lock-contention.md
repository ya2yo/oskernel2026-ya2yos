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

## 2026-07-25：MINIBUILD 路径命中与 inode 类型查询优化

### 新观测

`log.ans` 已完成本轮定向 MINIBUILD，输出 `BUILDSTORM_DEBUG_MINIBUILD ok`、
`BUILDSTORM_DEBUG_CASE name=minibuild ok` 和 `shutdown!`，无 `panic/TFAIL/TBROK`。
最终 perf 快照为：`path=4318737 us/1700`、`open=3599279 us/763`、
`read=3742446 us/3896`、`write=3631424 us/1342`、`stat=2613611 us/1081`，
`lseek=130216 us/8396`；其中 `lseek` 的 `type_check=56150 us/8403`。
这些是单次运行的累计观测，未与同镜像、同缓存状态的旧内核形成 A/B，不能据此宣称
端到端加速比例。

### 根因与修复

缓存 inode 的 `open` 路径原先先调用 `FsIndex::has_inode()`，随后再次调用
`find_inode_idx()`；命中后还会重复登记路径 alias。对 EXT4 inode 而言，alias 更新和
`types()` 查询都可能进入全局 `EXT4_OP_LOCK`，使纯缓存命中仍重复获取文件系统锁。

本轮做了三项局部优化：

- `FsIndex::find_inode_idx()` 只进行一次索引读取，不在每次命中时重复写 alias；新 inode
  绑定仍由 `insert_inode_idx()` 负责登记 alias。
- `open_inner()` 直接复用一次缓存查找结果，移除 `has_inode` 双查，并在一次
  `inode.types()` 结果上完成目录、设备、写权限相关的类型分支。
- `Ext4Inode` 保存创建时的不可变 `InodeType`，`types()` 变为无锁只读查询；路径、大小、
  fstat、读写和删除等仍使用原有 EXT4 锁和恢复逻辑。

### 验证

`cargo fmt --manifest-path os/Cargo.toml -- --check`、`git diff --check` 和
`make perf TARGET_ARCH=riscv64` 通过，后者生成带 perf 统计的 RISC-V 内核；构建过程仅有
既有 smoltcp warning。随后一次独立 QEMU 启动因宿主 `/var/tmp` 临时文件权限失败，获准的
第二次运行被中断，未取得可归因于本改动的新 A/B wall-clock 样本；文中 perf 数值来自现有
`log.ans` 的成功 MINIBUILD 快照。

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

## 2026-07-25：收窄读取与目录枚举的 EXT4 锁临界区

### 新观测

定向 `cagent fs-search` 的最终 perf 快照显示，44 次 `read` 累计约 `767462 us`、最大单次
`340045 us`；EXT4 锁等待 `2818072` tick、持有 `11312293` tick。`read_active` 只有约
`12925 us`，说明额外时间主要发生在阻塞/锁竞争，而不是用户缓冲复制。

### 根因与修复

`Ext4Inode::read_at()` 在底层读取完成后仍持有全局锁执行只读动态库字节补丁；
`read_dentry()` 还在锁内序列化目录项、查询挂载标志并更新 atime。上述工作不访问 lwext4，
会把纯内存开销放大为所有 hart 的锁等待。

本轮将 `read_at()` 的锁范围限制为缓存和 lwext4 descriptor 访问，将动态库补丁移到解锁后；
`read_dentry()` 只在 `read_dir_from()` 期间持锁，目录项打包和挂载表查询移到锁外，atime
更新改为单独的短 `set_timestamps()` 操作。没有放宽 lwext4 的全局 SMP 安全锁。

### 验证

RISC-V perf 内核使用同一 final-2026 镜像的临时 qcow2 overlay 重复运行两次，`fs-search`
分别为 `pass 695` 和 `pass 722`；最终快照的 `read` 累计为 `590669/624178 us`，锁等待为
`914651/973686` tick，锁持有为 `8823483/9019860` tick。两次均正常 `shutdown!`，无
`panic/TFAIL/TBROK`；宿主 wall-clock 分别约 `2.31/2.30 s`。这是定向样本，不代表完整
BuildStorm 446 crate 的加速比例。RISC-V 与 LoongArch64 release 构建均通过。

## 2026-07-25：MINIBUILD `lseek` 热路径计时

### 新观测

本轮根据 `debug.ans` 追踪的是 `buildstorm::minibuild::run()`，不是用户态总耗时。
日志共 64389 行，`BUILDSTORM_DEBUG_MINIBUILD ok` 位于约第 64333 行；按 PID 和 syscall
聚合后，PID 64 出现 8329 次 `Lseek`、3511 次 `Read` 和 1231 次 `Write`。其中大量
`Lseek` 集中在 cargo/rustc 处理归档或对象文件的连续 seek/read 区间，属于当前最明确的
高频候选，但 `debug.ans` 本身没有 tick，不能仅凭次数断言它占用了多少实际时间。

### 埋点

`OSFile::lseek()` 现在由 `LseekDurationGuard` 统计 VFS 实现总时间，并把以下阶段单独
聚合为 samples、total_ticks、max_ticks：

- `type_check`：`inode.path()`、特殊节点索引查询和 `inode.types()` 回退；
- `size`：`SEEK_END` 的 `inode.size()`，包括已知长度缓存和 EXT4 查询；
- `sparse`：`SEEK_DATA`/`SEEK_HOLE` 探测。

syscall 层的 `lseek` 仍单独统计完整 syscall 时间，因此可以用 `lseek - impl` 估算 fd
查找和 syscall 分发开销。`lseek` 已从泛化 path duration 桶独立出来，报告仍限频且不打印
逐调用日志。

### 验证与边界

`cargo fmt --manifest-path os/Cargo.toml -- --check`、`git diff --check`、
`make perf TARGET_ARCH=riscv64` 通过；普通 `make TARGET_ARCH=riscv64` 同时完成 RISC-V
和 LoongArch64 release 构建。尝试使用 `/tmp` 作为临时目录运行 guest，QEMU 仍因宿主
`/var/tmp` 只读而在启动前失败，因此尚未取得新的 `[perf] lseek_duration` 快照，也不把
`lseek` 认定为已证实的时间占比或宣称完整 BuildStorm 加速。

## 2026-07-26：全局锁可睡眠等待与只读描述符复用

### 新观测

此前的 Cargo 并发样本中，lwext4 仍由 `spin::Mutex` 保护。该锁会跨越文件读取和块设备
访问，持锁可达毫秒量级；其他 hart 在此期间持续忙等。旧样本在约 `37815` 次 EXT4 读取时，
累计锁等待/持锁分别为 `1401679883/986584484` tick。分类统计中 `read_at`、`find`、`fstat`
均有显著等待，且 QEMU 的 guest 时间在激烈竞争窗口中多次停滞，表明忙等会放大宿主调度压力。

### 修复

`Ext4OpLock` 仍保持一个 lwext4 全局互斥门闩，不允许并发进入第三方库；但任务上下文在
首次 `try_lock()` 失败后会注册到 `PollSet`，通过 `block_on()` 进入 `Blocked`。释放锁时先
释放原始 mutex，再唤醒 waiter，避免解锁与注册之间丢失唤醒；没有当前任务的启动期继续使用
自旋回退，避免早期初始化依赖调度器。

同时将 `Ext4File::file_open_inner()` 的已打开快路径改为直接比较 `path_str()`、`flags` 和
`has_opened`。常见的重复 `O_RDONLY` 读取不再在 EXT4 锁内构造两次比较用 `CString`；路径或
打开标志变化时仍执行原有 `ext4_fopen()`，因此不改变 descriptor 切换语义。

### 验证与边界

新 RISC-V `log.ans` 在 `38943` 次 EXT4 读取时记录全局锁等待/持锁
`951568610/677557464` tick；与旧样本相近读取量相比均下降。分类统计为：

- `read_at`：`70561126/16952778` us；
- `find`：`16476614/12595879` us；
- `fstat`：`5231028/12868936` us。

Cargo 已推进到 `3/446`，日志中没有 `panic`、`TFAIL`、`TBROK` 或新的 EXT4 错误，但 QEMU
被外层终止，不能根据单个未完成样本宣称端到端加速比例。`make perf TARGET_ARCH=riscv64`、
`make perf TARGET_ARCH=loongarch64` 和 `git diff --check` 通过；构建仅有既有 smoltcp 未使用项
警告。直接描述符复用快路径完成双架构编译，尚未形成独立运行样本。

## 2026-07-26：EXT4 锁 FIFO 单唤醒交接

### 新观测

最新 `log.ans` 仍在 Cargo `3/446` 时被终止，但已显示全局锁 `91481` 次获取中，`read_at`
分类累计等待约 `70.56` 秒，明显高于其持锁约 `16.95` 秒。现有可睡眠锁使用
`PollSet::wake()`；该函数会唤醒队列中的全部 waiter。Cargo 并发读取时，这些任务会同时被
调度、抢同一个 lwext4 mutex、失败后再睡眠，形成不必要的 wake/retry 风暴。

### 修复

`PollSet` 改为有界、去重的 FIFO waker 队列，新增 `unregister()` 和 `wake_one()`：

- 同一任务重复 poll 不会写入多个 waker；队列满时仅唤醒最老 waiter，保持原有的有界语义。
- EXT4 的二次 `try_lock()` 成功后会撤销已登记的 waker，避免它在随后解锁时占据一次无效交接。
- `Ext4OpGuard::drop()` 在释放 primitive mutex 后仅唤醒一个 waiter；该 waiter 后续获得或再次
  等待锁，下一次解锁再交接给队列中的下一个任务。

lwext4 仍由唯一的 `spin::Mutex` 串行保护，未改变第三方块缓存/路径 API 的非 SMP 安全假设；
启动阶段无当前 task 的自旋回退也保持不变。

### 验证与边界

`rustfmt --edition 2021 os/src/utils/poll.rs os/src/fs/ext4_lw/mod.rs`、
`git diff --check`、`make perf TARGET_ARCH=riscv64` 和
`make perf TARGET_ARCH=loongarch64` 均通过，仅有既有 smoltcp 未使用项 warning。

`timeout 90s make run TARGET_ARCH=riscv64` 成功启动 8 hart QEMU，并进入
`buildstorm-compile` 的 `pre-build tg-xtask` 阶段；外层 timeout 结束前未观察到 panic。
该运行没有产生可与旧 `log.ans` 对齐的后续 perf 快照，不能据此报告加速比例或完整 BuildStorm
通过。后续应以相同镜像和 timeout 获得至少一个 Cargo 稳定阶段的累计锁统计，再比较唤醒风暴的
实际影响。
