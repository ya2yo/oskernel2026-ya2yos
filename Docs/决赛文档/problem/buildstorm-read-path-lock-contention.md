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

## 2026-07-26：普通 `read()` 复用文件页缓存

### 新观测

根目录 `log.ans` 不是死锁或编译器报错：guest 在 `t=287411ms` 时只有 Cargo `3/446`，随后
被外层终止。最终统计为 `read=3919`、EXT4 读取 `40293` 次和 `227390418` 字节，
`ext4_read_lock` 累计等待 `70552459us`、持锁 `22801924us`；日志没有 `panic`、`TFAIL`
或 `TBROK`。现有 `FILE_PAGE_CACHE` 命中统计主要来自 mmap 缺页，普通 `OSFile::read()`
仍反复调用 `inode.read_at()`，因此 Cargo/rustc 的重复小文件读没有共享页缓存。

### 根因

跨页 `read()` 虽已合并为一次连续的 EXT4 读，但每个 syscall 的结果只写入用户缓冲区，
没有发布到共享文件页缓存。并行 Cargo 任务随后再次读取相同的归档、元数据和构建脚本时，
仍需进入 lwext4 的全局串行锁；这表现为吞吐极低和锁等待累计增长，而不是任务在锁上永久
互相等待。

### 修复

`FilePageCache` 新增 `read_cached_at()` 和 `insert_read_range()`：命中时在共享锁下复制
所有完整页，缺页则回退一次原有 `inode.read_at()`，只发布完全覆盖的页，避免把部分页或
未初始化尾部暴露给后续读取。`OSFile::read()` 仅对普通文件、文件不超过 1MiB 且请求为
`PAGE_SIZE < len <= 64KiB` 的读启用该路径；单页、大读、特殊文件继续使用原有零拷贝/流式
分支。已有 write、truncate、rename 和路径范围失效逻辑继续清除缓存，保持写后可见性边界。

### 验证与边界

`cargo fmt --manifest-path os/Cargo.toml -- --check`、`git diff --check`、
`make TARGET_ARCH=riscv64 build-arch`、`make TARGET_ARCH=loongarch64 build-arch` 和
`make perf TARGET_ARCH=riscv64` 均通过（仅有既有 smoltcp unused warning）。使用 `/tmp`
qcow2 叠加盘运行 RISC-V 定向 BuildStorm 180 秒，无 panic/TFAIL/TBROK；Cargo 从 `0/446`
推进到 `1/446`，约 `t=128434ms` 的快照为 EXT4 读取 `26357` 次、锁等待 `46747937us`、
持锁 `10686619us`。这不是同镜像严格 A/B，完整 446 crate 和正式评分仍未完成，不能据此
宣称端到端加速比例。

## 2026-07-26：Vfork hart 分布、跨 worker VFS 缓存与单页读缓存

### 新观测

最新 `log.ans` 已不再出现死锁、`SIGSEGV`、`panic`、`TFAIL` 或 `TBROK`，但 Cargo 在 8 hart
并行后仍受 lwext4 全局锁限制。上一份样本首次出现 `Building 11/446` 在
`t=518588ms` 之后；启用单页 read 缓存后的新样本在 `t=302597ms` 后已进入 `11/446`。两次
运行不是严格 A/B，但新样本在更短的 guest 时间推进到相同阶段，且同阶段的 EXT4 读取/读锁
统计由旧样本 `45623/669455984us/64669037us` 降为 `33278/368941615us/35626467us`
（依次为 reads、读锁等待、读锁持有）。

新样本的剩余主要锁等待是 `find=278324986us` 和 `fstat=192636911us`，而不是 hart 没有启动。
启动日志仍确认 HART0 至 HART7 都已上线；`-smp 8` 已生效。

### 修复

- Cargo 的 `posix_spawn` 使用 `CLONE_VM|CLONE_VFORK`。父任务在子任务可运行前已进入
  `VforkBlocked`，子任务在恢复父任务前会 `execve()` 或退出，因此该类非线程子进程改用
  `Process::new()` 的 pid round-robin hart 分配；普通可与父并发的 `CLONE_VM` 线程仍固定在
  父 hart，避免没有远程 TLB shootdown 时跨 hart 共用地址空间。
- inode 与 dentry lookup cache 从 4096 提升到 32768 项，并取消“某个独立 fd table 的最后
  owner 退出即清空全局缓存”。缓存仍有硬上限；达到预算时先清 dentry，再回收不再被使用的
  inode，故不会无限增长，也不会以悬空父 inode 指针复用旧 dentry。
- `OSFile::read()` 的普通文件单页请求也接入现有 `FILE_PAGE_CACHE`。冷读加载完整页，命中后
  只复制缓存字节；文件大小上限从 1MiB 放宽到 8MiB。现有 write、truncate 与 rename 的页缓存
  失效继续生效。
- `Ext4Inode` 为普通文件缓存完整 `Kstat`；第一次查询和缓存未命中仍在 EXT4 锁内完成，命中则
  直接返回。读、写、truncate、rename、hard-link、unlink、chmod、chown 与显式时间戳更新都会
  失效；目录和特殊节点不缓存，以免目录项变化暴露旧元数据。

### 验证与边界

`make perf TARGET_ARCH=riscv64`、`make TARGET_ARCH=loongarch64` 和 `git diff --check` 通过，
仅有既有 smoltcp unused warning。`log.ans` 中的 303 s 运行覆盖了单页 read 缓存，不包含随后
加入的 `fstat` 缓存；后者目前仅完成双架构编译验证。完整 446 crate、正式评分和严格同配置
A/B wall-clock 尚未完成，不能据此承诺一小时内完成编译。

## 2026-07-27：复用首次路径查找的 EXT4 元数据

### 新观测

维护者提供的 360 秒 `log.ans` 仍未完成 BuildStorm：最后一条快照在
`t=322949ms`，Cargo 只到 `8/446`，没有 `BUILDSTORM_DEBUG_COMPILE`、`shutdown!`、
`panic`、`TFAIL` 或 `TBROK`。在相近文件系统工作量的 `t=130268ms` 快照中，普通文件
`find()` 已执行路径 `ext4_stat_get()`，随后 `FsIndex::insert_inode_idx()` 又因取得
`(st_dev, st_ino)` 调用 `inode.fstat()`；页缓存首读还会再次查询 `inode.size()`。这三步中
前两步的元数据来自同一个路径状态，却都要排队进入 lwext4 的全局锁。

### 修复

- `Ext4File` 新增 `inode_type_and_stat_at()`，一次路径查找返回 inode 类型与
  `ext4_inode_stat`；保留 `inode_type_at()` 作为只需类型的兼容包装。
- `Ext4Inode::find()` 对普通文件用该结果初始化 `stat_cache` 和 `known_size`。因此
  `FsIndex` 获取 identity、`fstat()` 与首个页缓存 EOF 检查可直接命中 VFS 侧缓存；写、
  截断、rename、chmod、chown 和显式时间更新仍按原逻辑失效缓存。目录、符号链接和特殊节点
  没有改为静态缓存，避免目录项或链接语义变化后返回旧元数据。
- `FsIndex::insert_inode_idx()` 仅在已有 canonical inode 获得真正的新路径（例如 hard link）
  时调用 `cache_path_alias()`；新建 inode 的初始 path 已在构造时记录，重复登记不再进入
  `EXT4_OP_LOCK`。陈旧 inode 的替换同样使用新对象已具备的初始 alias。

### 验证与边界

`cargo fmt --manifest-path os/Cargo.toml`、
`cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml`、`git diff --check`、
`make perf TARGET_ARCH=riscv64` 与 `make build-arch TARGET_ARCH=loongarch64` 均通过，构建
仅含既有 smoltcp unused warning。

维护者提供的三分钟 RISC-V `1.ans` 同样未完成（最高到 Cargo `5/446`，无结束或失败标记），
不能比较完整 wall-clock。与前一样本中读取量相近的快照相比：

| 指标 | 360 秒样本 `log.ans`，`t=130268ms` | 本轮 `1.ans`，`t=105564ms` |
| --- | ---: | ---: |
| EXT4 reads | 23,592 | 25,128 |
| `open` | 5,030 | 5,227 |
| `ext4_fstat_lock` samples | 8,929 | 6,028 |
| `ext4_fstat_lock` hold | 7.39 s | 4.11 s |
| `ext4_fstat_lock` wait | 4.22 s | 1.23 s |
| 全部 EXT4 lock hold | 58.76 s | 44.83 s |

该比较支持“重复元数据查询已被移除”的假设，但两次的 guest 时间、Cargo 阶段和宿主负载并不
完全相同；后续仍需使用同一镜像、相同 timeout、至少两次完整 BuildStorm 运行，才可报告最终
编译时间或加速比例。

## 2026-07-27：写回缓存活跃工作集驱逐

### 新观测

维护者提供的最新 RISC-V `3.ans` 是启用 EXT4 写入/rename 分类后的十分钟样本。最后一条快照
位于 `t=564168ms`，Cargo 只到 `9/446`，随后由外层 QEMU 终止；日志没有 `panic`、`ERROR`、
`TFAIL`、`TBROK`、`TEST GROUP END` 或 `shutdown!`，所以它反映的是低吞吐而不是功能失败。

该快照的全局 EXT4 锁累计等待/持有为 `2951.10 s/491.14 s`（跨任务累计，不能视为单一
wall-clock）。新增分类将此前的大部分 mutation 空洞收敛为：

| 类别 | samples | wait | hold |
| --- | ---: | ---: | ---: |
| `ext4_read_lock` | 38,240 | 852.65 s | 150.94 s |
| `ext4_find_lock` | 20,037 | 1026.97 s | 65.98 s |
| `ext4_fstat_lock` | 6,681 | 207.23 s | 26.28 s |
| `ext4_write_lock` | 5,188 | 546.53 s | 171.65 s |
| `ext4_rename_lock` | 19 | 9.73 s | 3.95 s |

`find` 仍是最大的排队来源，而 `write_at` 已是最大的已分类临界区，平均每次约 33 ms。相反，
`lseek` 实现累计仅 2.29 s，调度 dispatch 仅 0.44 s，`clone`/`execve` 活动时间也远小于文件
系统锁累计，均不是本轮主优化方向。

### 根因

`Ext4Inode::write_at()` 持有唯一的 `EXT4_OP_LOCK` 调用 `Ext4File::file_write_at()`。后者为了
合并小写，会把普通文件放入 whole-file write-back cache；旧实现只有 `FIFO_SIZE = 10`，且命中
缓存时不更新 FIFO 顺序。并行 Cargo/rustc 的多个 worker 交错写 `.d`、metadata 和临时 artifact
时，仍在增长的文件会因其它 worker 新建缓存而被驱逐。一次驱逐会在同一全局 EXT4 锁内重新打开
文件并将整个 byte cache 写回，因此下次写同一产物又要重新建 cache，形成“驱逐—整文件回写—重建”
抖动。此前为延迟删除文件做的 pin 只覆盖 `unlink` 临时文件，不覆盖这类正常 rename 发布的输出。

### 修复

- 将有界 write-back 工作集从 10 项提升到 32 项。每项仍受 `MAX_CACHED_FILE_SIZE = 16 MiB`
  限制，缓存数据上限为 512 MiB；RISC-V/LoongArch QEMU 分别配置 16 GiB/8 GiB 内存，未引入
  无界缓存。
- `file_write()` 与 `file_write_at()` 在命中、修改 byte cache 后把对应 pathname 移到队尾，
  使淘汰策略变成 LRU。仍活跃的 Rustc 输出不再被纯插入顺序错误驱逐；闲置产物保留原有的写回、
  成功后删除和错误重试行为。
- 未改变 lwext4 的全局串行保护、`write_back_cache_entry()` 的完整写回语义、稀疏文件禁用策略、
  rename 前 flush/discard，或延迟删除文件的 pin 行为。

本轮还把普通 `find()` 中 Rust 侧 `Arc<Ext4Inode>` 构造移到 metadata 查询的锁外，并加入
`write`/`rename` 两类低开销累计锁统计；这些统计仅在 `perf` feature 下输出，不会逐调用打印。

### 验证与边界

已执行 `cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml`、
`cargo fmt --manifest-path os/Cargo.toml -- --check`、`git diff --check`、
`make perf TARGET_ARCH=riscv64` 和 `make perf TARGET_ARCH=loongarch64`，均通过。构建只有既有
smoltcp unused-import/dead-code warning。

尚未运行新的 QEMU 样本：`3.ans` 生成于本次 LRU 改动之前，只能用于根因定位，不能作为加速
结果。后续应使用相同 `buildstorm::compile::run()` 入口和至少十分钟窗口，比较 Cargo 进度及
`ext4_write_lock` 的 samples/wait/hold；若 write hold 未明显下降，再对 `file_write_at()` 的
cache-hit、cache-build 与 eviction/write-back 分段计时，避免继续猜测。

## 2026-07-27：LRU 样本复核与 cached-parent miss 快路径

### 新观测

维护者提供的 `4.ans` 是已包含 whole-file write-back LRU 的十分钟 RISC-V 样本。最后一条
快照在 `t=593387ms`，Cargo 仍只到 `9/446`；日志同样没有 `panic`、`ERROR`、`TFAIL`、`TBROK`、
`TEST GROUP END`、`shutdown!` 或 BuildStorm 完成标记。因此 LRU 没有引入可见功能回归，但单凭
十分钟进度不能证明端到端吞吐已经改善。

`3.ans` 与 `4.ans` 的末尾分类统计如下。锁等待、持锁为所有 task 的累计值，不能与十分钟
wall-clock 一一对应；两次样本也不是严格的同一执行阶段 A/B。

| 指标 | `3.ans` | `4.ans` |
| --- | ---: | ---: |
| `ext4_find_lock` wait / hold | 1026.97 / 65.98 s | 1000.77 / 65.90 s |
| `ext4_read_lock` wait / hold | 852.65 / 150.94 s | 890.54 / 151.00 s |
| `ext4_write_lock` samples | 5,188 | 6,059 |
| `ext4_write_lock` wait / hold | 546.53 / 171.65 s | 763.78 / 183.57 s |
| 每次 `write_at` 平均持锁 | 约 33.1 ms | 约 30.3 ms |

写路径平均锁内时间约下降 8%，符合“活跃输出不再被纯 FIFO 顺序提前驱逐”的预期，但绝对
`write` 工作量更多、Cargo 进度相同，不能据此宣称整体编译加速。`find` 仍稳定地贡献约千秒
累计排队，说明下一步应减少查找失败后的额外路径探测，而不是继续无证据地扩大 dentry/FsIndex
的 32K 上限。

### 修复

普通 `find()` 在末级查询失败时，为兼容 Debian 路径中的中间符号链接，会调用
`resolve_intermediate_symlink()`，逐个前缀进入 lwext4 检查符号链接。`find_from_cached_parent()`
的调用点已经从 `FsIndex` 得到实际目录 inode，并使用该 inode 的真实路径加上一个末级名称。
在这种“已解析父目录 + 直接 child”查询中，若 child 不存在，前缀中不可能还有未解析的中间
链接；此前扫描必然失败，却额外占用未分类的 `EXT4_OP_LOCK`。

- `Inode` 增加默认的 `find_from_cached_parent()`，其他 VFS 后端继续走原有完整 `find()` 语义。
- `Ext4Inode` 对该接口使用私有 sentinel：只有直接 child 未命中时跳过中间链接回退扫描，直接
  返回 `ENOENT`。最终组件是符号链接时，仍读取链接并按原有递归路径解析；普通完整路径查找、
  `O_NOFOLLOW`、`O_UNLINK`、`O_DIRECTORY` 和最长链接深度的语义均保持不变。
- `open_inner()` 的 cached-parent helper 改调此接口。负 dentry 的缓存、`O_CREAT` 后续创建与
  父目录路径的真实化逻辑没有改变。

这项优化不会取消必需的 `inode_type_and_stat_at()`，故新的样本中 `ext4_find_lock` 的一次查询
计数未必立即降低；目标是削减 miss 之后原先落在 generic EXT4 lock 桶中的逐前缀扫描和其排队。

### 验证与边界

最新改动后执行了 `cargo fmt --manifest-path os/Cargo.toml`、
`cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml -- --check`、`git diff --check`、
`make perf TARGET_ARCH=riscv64` 与 `make perf TARGET_ARCH=loongarch64`，均通过。构建仅出现既有
smoltcp 的两个 unused-import warning 和一个 dead-code warning。

尚无含本次 cached-parent fast path 的新的 QEMU 样本；`4.ans` 只验证了其前的 LRU 改动。下一次
至少十分钟同入口采样应同时记录 Cargo 进度、`ext4_find_lock`、五类已分类锁以及全局减去已分类
后的 generic EXT4 wait/hold。若 generic 时间没有下降，应新增低开销聚合计数区分 root 全路径
查找、cached-parent child 查询、中间链接回退和末级链接递归，再决定是否改动更深层路径解析。

## 2026-07-27：Rustc rename 发布的全挂载 flush 尖峰

### 新观测

维护者提供的 `5.ans` 已覆盖 cached-parent miss 快路径，运行约 30 分钟至
`t=1775349ms`，Cargo 从 `4.ans` 同窗口约十分钟的 `9/446` 推进到 `33/446`。相近的早期快照中，
`5.ans` 在 `t=598865ms` 已到 `10/446`，而 `4.ans` 在 `t=593387ms` 为 `9/446`。这说明路径优化
没有引入功能异常，且有方向性收益；两个样本的阶段、I/O 工作量和宿主状态不完全相同，不能据此
报告端到端加速比例。`5.ans` 没有 `panic`、`ERROR`、`TFAIL`、`TBROK` 或完成标记。

新的分类还暴露出一个更大的尾部尖峰。`t=1641118ms` 到 `t=1689127ms` 之间，
`ext4_rename_lock` 的 samples 仅从 `51` 增至 `52`，但累计 hold 从 `55.480s` 跳到 `130.358s`；
即单次 rename 独占约 `74.878s`。同一窗口全局 EXT4 lock 的累计 wait 增加约 `402.882s`、hold
增加约 `85.221s`，Cargo 几乎没有新的 read/write syscall 进展。这是串行文件系统发布路径造成的
队首阻塞，不是普通 Rustc 计算阶段。

### 根因

Rustc 通过 rename 发布已写完的临时 rmeta/rlib。旧 `Ext4Inode::rename()` 为防止旧 pathname 的
whole-file byte cache 在以后回写并重建临时文件，调用 `flush_and_discard_path_cache()`；该函数在
写回 byte cache 后执行 `ext4_cache_flush(path)`。lwext4 的此 API 先由 path 找到 mount，再调用
`ext4_block_cache_flush()`，循环清空该挂载点的全部 dirty block，不是对该文件的定向 flush。
随后 `file_close()` 又经 `file_cache_flush()` 再调用一次 `ext4_cache_flush()`。这使一个 rename 在
Ya2yOS 的全局 `EXT4_OP_LOCK` 内替所有 Cargo worker 同步积压的块 I/O，正好匹配 `5.ans` 的 75 秒
长临界区。

### 修复

- `Ext4File` 把“写回 pathname 的 whole-file byte cache”和“强制挂载级 block-cache flush”拆分。
  新 `write_back_and_discard_path_cache()` 仍在 rename 前完成 byte cache 的 `ext4_fwrite()`、在成功后
  丢弃旧 pathname 的 cache bookkeeping；任何写回错误仍保留 cache 以便重试。
- `Ext4Inode::rename()` 改用该轻量接口，并用 `file_close_without_cache_flush()` 关闭已打开的
  descriptor。`ext4_frename()` 仍在写回后执行，且 pathname cache 在成功 rename 后继续清理源和目标。
- 数据已进入 lwext4 共享 block cache，rename 移动的是同一个 inode，故后续打开能立刻看见完整
  内容。Linux `rename(2)` 本身不提供 `fsync` 级持久化保证；本轮没有删除或弱化显式 sync/fsync
  路径，只是不再把每一次 Rustc 原子发布变成对整个 mount 的隐式 flush。

### 验证与边界

修改后执行 `cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml`、
`cargo fmt --manifest-path os/Cargo.toml -- --check`、`make perf TARGET_ARCH=riscv64` 与
`make perf TARGET_ARCH=loongarch64`，均通过；构建仅有既有 smoltcp 的两个 unused-import warning
和一个 dead-code warning。`git diff --check` 已通过。

`5.ans` 生成在这次 rename 修复之前，因此目前的运行期证据是根因定位而不是修复后的 A/B。下一次
至少十分钟样本应重点观察 `ext4_rename_lock` 的最大/累计 hold 是否不再出现数十秒跳变，同时对比
Cargo 进度和全局 EXT4 wait/hold；还应保留 Rustc 临时文件 rename 后立即读取的定向回归与完整
BuildStorm 回归。

## 2026-07-27：`6.ans` 验证 rename 尖峰消失并收敛 stat cache 失效

### `6.ans` 观测

维护者以同一 RISC-V final-2026、16 GiB、8 hart 的 `buildstorm::compile::run()` 入口提供了
210 秒 `6.ans`。日志没有 `panic`、`ERROR`、`TFAIL` 或 `TBROK`，但外层 timeout 前没有
BuildStorm 完成标记，不能视为完整编译通过。

`5.ans` 在 `t=1641118ms -> 1689127ms` 之间只有一次新的 rename，却使
`ext4_rename_lock` hold 从 `55.480325 s` 激增至 `130.358082 s`，即单次 `74.877757 s`。
取消 rename 中的 mount-wide flush 后，`6.ans` 的 185.991 秒快照为：

- Cargo 已显示 `8/446`；`5.ans` 在相近的 193.979 秒仍停在 `3/446`。两次未完成运行的
  工作量和缓存状态不构成严格 A/B，不能据此计算加速百分比。
- `ext4_rename_lock` 为 13 samples、wait `0.935056 s`、hold `0.570715 s`，平均持锁约
  `43.9 ms`；没有新的分钟级 rename 临界区。
- 主导等待已回到全局 lwext4 串行化的普通 I/O：`read/find/fstat/write` 的分类 wait 分别为
  `304.622128/167.477541/31.258808/24.866948 s`，对应 hold 为
  `32.796255/26.183333/9.141548/23.436165 s`。这些是多 hart 累计值，不能与 210 秒 wall-clock
  直接相加。

因此本次样本验证了 rename flush 优化的目标，同时指出下一步应继续减少可证明的 metadata
锁进入，而不能移除 lwext4 的全局非 SMP 安全门闩。

### regular-file `stat_cache` 修复

`Ext4Inode::read_at()` 原先在每次底层读后无条件调用 `invalidate_cached_stat()`，注释假定
lwext4 会在 `ext4_fread()` 中更新 atime。审计
`crates/lwext4_rust/c/lwext4/src/ext4.c:1656` 的 `ext4_fread()` 后确认其只读取 inode/block、
更新 descriptor 的 `fsize/fpos`，没有修改 atime 或提交 inode。Ya2yOS 的 atime 修改只通过
`set_timestamps()` 显式执行，且该路径会保留原有 cache invalidation。

无条件失效不会得到新的可见 metadata，却会令后续 `fstat()` 跳过已缓存的 `Kstat`，重新等待
`EXT4_OP_LOCK`。这与 `6.ans` 中 6,375 次 fstat 分类锁、31.258808 秒累计等待相符。

`os/src/fs/ext4_lw/inode.rs` 现保留读后的 regular-file `stat_cache`。写入、truncate、rename、
link/unlink 和 `set_timestamps()` 的既有失效操作未改变，所以 size、mode、owner、timestamps 或
路径实际变化时仍会重新查询。

### 验证与边界

- `cargo fmt --manifest-path os/Cargo.toml -- --check`：通过。
- `make perf TARGET_ARCH=riscv64`：通过；仅有既有 `smoltcp` warning。
- `make perf TARGET_ARCH=loongarch64`：通过；仅有既有 `smoltcp` warning。
- `git diff --check`：通过。
- `6.ans` 在 stat-cache 改动前生成，故它只验证 rename flush 优化；新的 stat-cache 路径尚未
  取得 guest A/B 或完整 BuildStorm 样本，不能宣称其具体加速比例。

## 2026-07-27：`7.ans` 的后段慢化不构成 stat cache 回归

### 现象

维护者提供的 `7.ans` 是启用 regular-file `stat_cache` 保留后的 RISC-V、16 GiB、8 hart
BuildStorm `buildstorm::compile::run()` 240 秒样本。日志最终显示 Cargo `7/446`，而 210 秒的
`6.ans` 在超时前已打印 `8/446`，所以“这次感觉更慢”有直接的表面依据。两个日志都没有
`panic`、`ERROR`、`TFAIL`、`TBROK`、`shutdown!` 或 BuildStorm 完成标记，均只是中途快照。

但按 Cargo 阶段标记对齐，前半段并未退化：

| 阶段标记 | `6.ans` | `7.ans` |
| --- | ---: | ---: |
| `Building 0/446` | 71.807 s | 69.730 s |
| `Building 5/446` | 185.991 s | 166.289 s |
| 日志末尾可见进度 | `8/446`（210 s timeout 前） | `7/446`（238.983 s） |

因此，`7.ans` 的慢化集中在 `5/446` 之后；这时 Cargo 正在编译 `serde_core`，随后到
`scopeguard`，并非全程吞吐下降。

### 分类数据

`7.ans` 从 `t=197096ms`（`Building 6/446`）到 `t=238983ms`（`Building 7/446`）的 41.887 秒内：

| 分类 | samples 增量 | wait 增量 | hold 增量 | 每次 hold |
| --- | ---: | ---: | ---: | ---: |
| `ext4_write_lock` | 3,398 | 82.582351 s | 20.343140 s | 5.99 ms |
| `ext4_fstat_lock` | 147 | 12.387939 s | 0.763626 s | 5.19 ms |
| `ext4_read_lock` | 2,336 | 104.042145 s | 13.976329 s | 5.98 ms |

这是一段明显的 artifact 写入/读取工作量上升：`write` 数量从 997 增至 4,395，而不是单次
write 临界区突然变长。相比之下，fstat 样本只从 6,099 增至 6,246；在更早的 `5/446` 标记，
`7.ans` 的 fstat 样本为 5,761，也低于 `6.ans` 对应快照的 6,375。故现有数据不支持“保留
stat cache 使 fstat 回退或放大了锁请求”的假设。

最后快照中，`ext4_rename_lock` 为 19 samples、hold `2.153465 s`；虽然普通 I/O 仍会竞争
lwext4 全局门闩，但没有重现 `5.ans` 一次 rename 持锁 74.878 秒的 mount-wide flush 尖峰。
file page cache 的 hit/miss 为 519,568/28,931（约 18:1），也没有显示页缓存命中明显退化。

### 结论与边界

保留 read 后的 regular-file `stat_cache` 不会直接进入 write 路径，且 `7.ans` 中 fstat 锁次数
没有异常增长；不能仅按两次 timeout 的最终 crate 序号将这项改动定性为回归。更合理的解释是
Cargo 依赖 DAG、worker 调度、镜像/宿主缓存状态使本次在 `serde_core` 的写入密集阶段停留更久。
当前 perf 计数缺少 write-back cache 命中、初始化和 LRU 驱逐的来源分类，因而也不能据此盲目
增大 LRU 或重写 write 路径。

后续应至少在相同 final-2026 镜像、RISC-V 16 GiB/8 hart、相同入口下重复两次 240 或 300 秒运行；
每次记录相同 Cargo 阶段及 read/find/fstat/write/rename 的 samples、wait、hold。若需要隔离
stat-cache 的影响，再在不覆盖维护者未提交改动的前提下，以临时可逆补丁跑一个仅恢复该一行失效的
对照样本。完成样本前不报告整体加速或回归百分比。

## 2026-07-29：`tmp_06.ans` 的 cold-inode fstat 与 FsIndex identity epoch

### 观测与取舍

`tmp_06.ans` 最后可用快照在 `t=587383ms`、Cargo `Building 21/446`，没有完成、`shutdown!`、
`TPASS`、`TFAIL` 或 `TBROK`，因此只能用于定位。

其中真正进入 EXT4 fstat 的慢路径为 `5615` 次、累计 `246.839s`；global gate 的 wait/hold 为
`188.586s/55.409s`，锁内 `ext4_stat_get=54.174s`，而 fstat-triggered sparse flush 仅 `0.477s`。
`sparse_buffered_write=7211` 是失效事件而不是实际 miss：真正被 fstat 消费的 sparse miss 仅 10 次、
`0.591s`。实际 miss 则以 `cold_inode=5091` 次、`29.164s` 为主，FsIndex reclaim/rebuild 都为 0。

故本轮不放松 sparse/direct write 的 Kstat 失效，也不按 `known_size` 伪造 `st_blocks`、mtime/ctime。
优化目标是可证明安全的 lookup identity 重复验证。

### 修复与安全边界

`Ext4Inode` 在捕获 immutable `(st_dev, st_ino)` 时同时记录 `EXT4_IDENTITY_EPOCH`。成功 `unlink`、
`rename`，以及 delayed unlink 最后关闭时成功的 `file_remove()` 推进 epoch；这些都是 inode 可能离开
namespace、以后被 lwext4 复用的边界。

`FsIndex::inode_matches_key()` 在 identity 相同且 epoch 未变化时直接复用 canonical inode，避开一次
live `fstat()`；epoch 不匹配、后端没有 epoch 证明或 identity 不同仍完全走旧 live probe/replacement。
因此已 unlink 后 inode number 被复用时不会被错误接受。改动不新增锁、不移动 lwext4 I/O，也不改变
sparse flush、write、rename、unlink 或用户可见 metadata 语义。

`vfs_lookup` 新增 `fsidx_identity_epoch_hit`、`fsidx_identity_live_probe` 和
`fsidx_identity_stale_replace` 三个 relaxed 聚合计数，分别记录免 probe、保守 probe 和拒绝 stale 的次数。

### 验证与边界

已执行 `cargo fmt --manifest-path os/Cargo.toml`、`git diff --check`、默认 `make`（双架构 release）、
`make log TARGET_ARCH=riscv64` 与 RISC-V/LoongArch64 `make perf`，均通过；仅有既有 Cargo config、
vendored `smoltcp` 和 `ipi_sent` warning。

RISC-V final-2026 raw 镜像用 `-snapshot` 直接启动，未改写维护者的 `disk.img`；guest 输出
`BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok`。120 秒 timeout 前最后可见的 `t=98093ms` 快照为
epoch hit `753`、live probe `18`、stale replacement `0`，无 panic/ERROR/TFAIL/TBROK。该 guest 未完成
BuildStorm/`shutdown!`，故不报告端到端加速，也未覆盖长时 inode reuse 压力。

## 2026-07-29：`tmp_07.ans` 对 identity epoch 的后续验证

### 结果是否符合预期

符合。`tmp_07.ans` 继续使用同一 BuildStorm 路由，已输出 `BUILDSTORM_TOOLCHAIN ok` 和
`BUILDSTORM_MINIBUILD ok`，未出现 `panic`、`ERROR`、`TFAIL` 或 `TBROK`。最后一个完整 perf 快照为
`t=583585ms`；其后的 Cargo 状态仍继续到 `Building 30/446`，但没有完整 BuildStorm、Summary 或
`shutdown!`，因此该样本只能验证路径行为和阶段性进度。

新增计数为 `fsidx_identity_epoch_hit=819`、`fsidx_identity_live_probe=18`、
`fsidx_identity_stale_replace=0`。前者按定义就是已免除的 live `fstat()` identity probe；后两者说明
发生 namespace epoch 变化时没有错误地继续采用 immutable identity，而是保留了 18 次旧的安全 probe。
正常 BuildStorm 没有恰好撞上 inode number reuse，故 `stale_replace=0` 是预期现象，不能替代专门的
reuse 压力回归。

| 末尾完整快照 | `tmp_06` | `tmp_07` |
| --- | ---: | ---: |
| guest 时间 / Cargo 阶段 | `587383ms` / `20/446` | `583585ms` / `29/446` |
| FsIndex identity 快路径 / 保守 probe / stale reject | 不适用（改动前） | `819 / 18 / 0` |
| `ext4_fstat_lock` samples / wait / hold | `5615 / 188.586 / 55.409 s` | `5649 / 185.422 / 53.645 s` |
| 实际 `ext4_fstat` / `ext4_stat_get` | `246.839 / 54.174 s` | `241.139 / 52.268 s` |
| fstat-triggered sparse flush | `0.477 s`（10 batch） | `0.560 s`（14 batch） |
| `ColdInode` 实际 miss | `5091` | `4976` |

该阶段进度和计数方向支持本轮优化：在相近 guest 时间内，`tmp_07` 工作量更多，仍避免了 819 次串行
identity probe，fstat wait/hold 和 `ext4_stat_get` 累计值没有恶化。它不是严格 A/B：Cargo 依赖调度、
文件读写量和 host/QEMU 状态不同，且两次运行都没有完成；不得将 `20 -> 29/446` 或累计微秒差异解释为
全量 BuildStorm 的加速比例。

### 下一轮取证与修复方向

不修改 sparse/direct write 的 `Kstat` 失效。`tmp_07` 的 5649 次实际 fstat 中，`ColdInode=4976`
（约 88%），而 sparse flush 仅占 `0.560s`；当前主成本仍是唯一 lwext4 gate 的排队，fstat/read/find
累计 wait 分别为 `185.422/722.397/261.410s`。下一轮先添加低开销归因，按 regular file、directory、
special node 以及“携带 lookup stat 构造”/“`Ext4Inode::new()` 无 lookup stat 构造”拆分 ColdInode。

只有确认某一类 metadata 在语义上可安全复用后，才考虑为该类缩短路径或保留缓存；目录的 mtime/ctime
会受子项变更影响，不能把普通文件的 `Kstat` 方案直接推广。与此同时新增一个 unlink-open-close-recreate
循环的 inode-number reuse 回归，要求 epoch mismatch 仍走 live probe，并在实际复用时观察到 stale
canonical 被拒绝；在此之前 `stale_replace=0` 只表示真实工作负载未覆盖该分支。

## 2026-07-29：10 分钟样本前的 ColdInode 二维归因

### 目的

`tmp_07` 的 `ColdInode=4976/5649` 仍不足以判断下一步能否安全复用 metadata。已有 regular-file
`stat_cache` 会在 `new_with_stat()` 中由 lookup 的 `ext4_stat_get()` 预填，而 directory 和 special node
为保持 mtime/ctime、目录内容与特殊文件语义，故意不沿用该缓存。因此先细分实际慢调用，不能直接扩大
`Kstat` cache 的适用范围。

### 实现

perf build 新增一行：

```text
[perf] ext4_fstat_cold_inode regular_lookup_stat=... regular_no_lookup_stat=... \
       directory_lookup_stat=... directory_no_lookup_stat=... \
       special_lookup_stat=... special_no_lookup_stat=...
```

只在某个 `Ext4Inode::fstat()` 已经过两次 cache check、真正将以 `ColdInode` 原因进入
`Ext4File::fstat()` 时记录一次 `Relaxed` 原子计数；fast-cache hit、写入/metadata 失效引起的其他
miss，以及 alias-recovery retry 均不计入。分类标准为：

- `regular`：`InodeType::File`；`directory`：`InodeType::Dir`；其他类型均归 `special`。
- `*_lookup_stat`：包装对象由 `new_with_stat()` 构造，pathname lookup 已提供 stat；
  `*_no_lookup_stat`：对象由 `Ext4Inode::new()` 构造，未携带 lookup stat。

每个 perf 快照必须满足六个字段之和等于同一报告中的
`ext4_fstat_inner_duration cold_inode(samples=...)`。若不相等，应先检查统计接入而不是解释比例。

### 待运行样本与判读

已通过 RISC-V 和 LoongArch64 `make perf`；没有因本次统计变更自行启动 guest。维护者运行同入口的
10 分钟 BuildStorm 后，应提取最新完整快照的上述一行、`cold_inode(samples=...)`、fstat/read/find lock
统计、Cargo 阶段以及 `panic/ERROR/TFAIL/TBROK/Summary/shutdown!` 标记。

若 directory/special bucket 主导，则保留它们的无 stat cache 语义并改查路径/identity；若
`regular_no_lookup_stat` 主导，才追踪该构造点能否安全携带 lookup metadata；若
`regular_lookup_stat` 非零，则首先核查普通 lookup stat 是否在首次 fstat 前被异常丢失。无论结果如何，
sparse/direct write 的完整 metadata 失效不在本轮候选范围内。

## 2026-07-29：`tmp_08.ans` 验证 ColdInode 来源并实现目录一次性 lookup stat

### `tmp_08` 结论

`tmp_08.ans` 已输出 `BUILDSTORM_TOOLCHAIN ok` 和 `BUILDSTORM_MINIBUILD ok`，没有匹配到
`panic`、`ERROR`、`TFAIL` 或 `TBROK`。它没有 Summary、`shutdown!` 或完整 BuildStorm，因此仍只是路径
归因样本；最后完整 perf 快照为 `t=565553ms`，其后的 Cargo 状态仅到 `Building 17/446`，不能与
`tmp_06/tmp_07` 的不同调度和工作量作端到端比较。

末尾 `ColdInode=4596` 的六项细分为：

| actual ColdInode fstat 来源 | 样本数 |
| --- | ---: |
| regular lookup-stat / no-lookup-stat | `0 / 0` |
| directory lookup-stat / no-lookup-stat | `2758 / 1838` |
| special lookup-stat / no-lookup-stat | `0 / 0` |

六项和为 `2758 + 1838 = 4596`，与同一快照
`ext4_fstat_inner_duration cold_inode(samples=4596)` 相等；前面的各快照也保持这个关系。故实际底层
ColdInode fstat 全是目录，其中约 `60.0%` 来自 pathname lookup 已得到 `ext4_stat_get()` 结果的
`new_with_stat()`，剩余约 `40.0%` 来自 `Ext4Inode::new()`。同一快照的 `ext4_stat_get` 为 `51.957s`，
fstat-triggered sparse flush 仅 `0.102s`，继续放宽 sparse/direct metadata 失效既不能消除主因，也会破坏
`st_blocks` 正确性。

### 实现：一次性而非目录 stat cache

目录的 mtime/ctime 会随子目录项 create/link/unlink/rename/symlink 变化，读取目录还可能更新 atime，因此
不能将普通文件的持续 `Kstat` cache 扩展到目录。本轮实现以下保守快路径：

1. `find()` 在持有 `EXT4_OP_LOCK` 的同一临界区取得目录 `ext4_inode_stat`、identity epoch 和新的
   mount-wide `EXT4_DIRECTORY_STAT_EPOCH`；新目录 wrapper 仅保存转换后的 `(Kstat, epoch)` 一次性快照。
2. `fstat()` 在现有 regular-file cache 检查之后、取得 `write_state/io_state/EXT4_OP_LOCK` 之前，短暂取得
   `stat_cache` 写锁并 `take()` 该快照。epoch 相等时直接返回，两个并发 fstat 也只能有一个消费成功；快照
   用过一次后，后续 fstat 保留原有 live lwext4 路径。
3. 成功 create、`create_dir_fast`、rename、hard link、symlink、unlink、delayed-unlink 的 `file_remove()` 都
   推进 directory epoch；目录自身成功 `set_timestamps()`、`fmode_set()`、`owner_set()` 同样推进。普通文件
   write/truncate、dense/sparse buffer 和 FsIndex identity epoch 均未改变。

directory epoch 与 inode-reuse 的 `EXT4_IDENTITY_EPOCH` 独立：前者的无关目录修改只会保守丢弃一次快照，
不会降低 FsIndex identity fast path 的命中。fstat 快路径不取得 lwext4 gate；它在 Acquire epoch load 处线性化，
并发但随后完成的目录修改可按 fstat 先于该修改观察，已经完成的修改则会使旧快照被丢弃并回退 live fstat。

perf 新增两项：

```text
[perf] ext4_fstat_path lookup_directory_stat(samples=... total_us=... max_us=...)
[perf] ext4_fstat_directory_lookup_stat epoch_miss=...
```

前者是一次性 terminal fast path，不是持续 cache hit；后者是 lookup 与首次 fstat 之间已有目录 metadata
变化时被安全丢弃的快照数。下一轮同入口样本应同时比较这两个字段、`actual_ext4_fstat`/`ext4_fstat_lock`、
ColdInode 六类及 `panic/ERROR/TFAIL/TBROK/Summary/shutdown!`。命中数量不必等于 2758，因为只有实际调用
首次 fstat 的目录会消费快照，且 epoch miss 必须回退慢路径。

### 验证边界

已执行 `cargo fmt --manifest-path os/Cargo.toml`、
`cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml`、`git diff --check`、
`make perf TARGET_ARCH=riscv64`、`make perf TARGET_ARCH=loongarch64` 及
`make TARGET_ARCH=riscv64`，均通过；最后再次运行 RISC-V `make perf`，使 `kernel-rv` 保持 perf 版本。
构建仅有既有 Cargo config 弃用、vendored `smoltcp` 与 release 的 `ipi_sent` warning。本轮按维护者的
10 分钟测试安排未启动新的 QEMU，故尚无 guest 运行期命中率或完整功能回归可报告。

## 2026-07-30：`tmp_17/18` dentry 结论与目录 epoch stat cache

### dentry 取证

`tmp_17.ans` 的末尾快照位于 `t=539592ms`、Cargo `Building 29/446`。新增的路径统计为
`path_index_hit=31517`、`dentry_positive_insert=2013`、`dentry_positive_hit=0`、
`dentry_negative_hit=3937`、`dentry_miss=4592`、`dentry_parent_miss=4638`；dentry clear 和容量淘汰均为零。
正目录项确实已写入，但普通成功路径会先被 `FsIndex` 的完整 pathname 命中截获，因此不再经过 dentry lookup。
负目录项仍避免重复 ENOENT 查找。结论是不恢复正 dentry fast path，避免为不会命中的缓存增加语义风险。

### 目录 stat cache

`tmp_17` 同一快照的实际 `ext4_fstat` 为 `4919` 次，`ext4_fstat_lock` 的 wait/hold 为
`219294389/59343405 us`。目录 lookup 已在全局 gate 内取得 stat，却只能被首次 fstat 一次性消费；随后稳定目录的
fstat 会重复进入 lwext4。实现将该 snapshot 改为按 `EXT4_DIRECTORY_STAT_EPOCH` 有效的缓存，实际 fstat 的新结果也会
回填。所有目录项变更和目录自身 metadata 变更，以及 read_dentry 的 atime 更新，均在成功后推进 epoch；因此缓存不跨
已完成的可见 metadata 修改。fstat 仍在 epoch load 处线性化，失配时保持原 live fstat 回退。

`tmp_18.ans`（`t=566931ms`、Cargo `Building 32/446`）有 `19457` 次 stat syscall、`3653` 次实际
`ext4_fstat`，fstat lock wait/hold 为 `175237416/52576106 us`。但日志仍打印旧的
`lookup_directory_stat(samples=2251, ...)`，而当前 `bdd7e30b` 内核使用 `directory_epoch_cached` 与
`ext4_fstat_directory_stat`；故该运行没有装载目录 epoch cache，不能将其中的 2251 次一次性 lookup-stat 返回归因于
新实现。该日志仅保留为旧路径基线。两份均有 `BUILDSTORM_TOOLCHAIN/MINIBUILD ok`，未见
panic/TFAIL/TBROK/ERROR；都未完成 compile、END 或 shutdown。

### 下一步

`tmp_18` 的 `read_bypass_file_ops=11252`、`read_bypass_file_bytes=74614810` 说明 8 MiB 文件准入阈值已绕过可观
读量，但全局 file page cache 无容量边界，不能直接扩大阈值。P9 已改为带固定上限的准入扩展，见下一节；其运行期
效果仍须用新内核样本验证。

## 2026-07-30：P9 有界大文件文件页缓存

### 基线与取舍

旧 `tmp_18.ans` 末尾的 `mmap_miss=21167`、`read_page_miss=24919`、`readahead_pages=22070` 合计约 68K 页，
而超过 8 MiB 的 regular-file read 已旁路 `11252` 次、`74614810 B`。这表明扩大准入有可验证的候选工作量，但不证明
每个冷页会再次命中。由于原 cache 不会主动逐出，直接移除 8 MiB 限制会让长时编译不受控制地占用页帧，未采纳。

### 实现与语义边界

`MAX_PAGE_CACHED_READ_FILE_SIZE` 调整为 32 MiB；`FilePageCache` 新增 96K 页的全局常驻上限（当前 4 KiB 页约
384 MiB）。冷页在分配/发布前由 CAS 预留一个 page slot：`insert_read_range()` 的完整 read 页以及
`get_or_load()` 的 fault/readahead 页都受同一上限约束；并发发布同一 key 未成功时和 frame 分配失败时释放 reservation。
write range invalidation、truncate、rename/unlink 的全路径 invalidation 则按实际移除页数递减计数。

容量满时 faulting/read page 仍由已完成的 inode read 返回，只跳过共享 cache 发布；因此不会把暂时的容量压力暴露为
`ENOMEM`、短读或陈旧数据。改动不移动 `EXT4_OP_LOCK`、不改变两页顺序预读上限，也不改变 write/rename/truncate 的
缓存失效语义。

perf 报告新增 `file_cache_capacity resident_pages=<...> max_pages=98304 capacity_bypass_pages=<...>`。其中 bypass 只表示
冷页未被保留，不能单独作为 read I/O 或性能退化结论。

### 验证与下一次采样

已执行 `cargo fmt --manifest-path os/Cargo.toml --all -- --check`、`git diff --check`、
`make special_make TARGET_ARCH=riscv64`、`make special_make TARGET_ARCH=loongarch64`、
`make perf TARGET_ARCH=riscv64` 及 `make perf TARGET_ARCH=loongarch64`，均通过；最后再次重建 RISC-V perf 内核。构建仅显示
已有 Cargo config 弃用、vendored `smoltcp` 未使用项和非 perf release 的既有 `ipi_sent` 未使用变量 warning。本轮未启动新的
长 QEMU。

下一次测试必须使用上述 RISC-V perf 构建产物，并在日志中同时看到 `file_cache_capacity`、
`directory_epoch_cached` 和 `ext4_fstat_directory_stat`。仍以同一 Cargo 检查点比较
`read_bypass_file_*`、`resident_pages/capacity_bypass_pages`、页缓存 hit/miss、`ext4_read_data_lock` 与
`ext4_find_lock` 的 wait/hold；没有 `BUILDSTORM_COMPILE`、END、`shutdown!` 的 timeout 样本不作为端到端加速结论。

## 2026-07-30：`tmp_20.ans` 验证 P9 并准备 P10

### 运行期结果

`tmp_20` 包含 `file_cache_capacity`、`directory_epoch_cached` 与 `ext4_fstat_directory_stat`，确认运行的是 P9/RISC-V
perf 内核。它有 `BUILDSTORM_TOOLCHAIN/MINIBUILD ok`，未见 `panic/TFAIL/TBROK/ERROR`，但最后只打印 Cargo
`Building 27/446`，没有 compile/END/shutdown，仍是中途样本。

同为 `Building 26/446` 的 `tmp_18`/`tmp_20` 快照显示，原 8 MiB 准入造成的 `read_bypass_file` 从
`11060 ops / 72744122 B` 降至 `45 ops / 37440 B`；新 cache 的 resident 为 `60092 / 98304` 页，
`capacity_bypass_pages=0`。这直接验证 32 MiB 准入与容量边界的行为，但不证明每个新增页都带来可观复用。

同一检查点 `ext4_read_data_lock` 的 wait/hold 从 `440.983/36.495 s` 降至 `293.473/23.890 s`，
`ext4_find_lock` 从 `255.732/54.484 s` 降至 `204.783/44.803 s`；末尾 `t=561110ms` 仍为
`453.407/28.184 s` 与 `238.507/58.840 s`。两个样本的 Cargo 依赖交错不同，且没有完成，故这些数值只支持保留 P9，
不报告吞吐百分比。最后 resident `60294` 页、capacity bypass `0`；未尝试继续扩大 32 MiB 或 96K 页参数。

目录 epoch cache 也实际运行：末尾 `directory_epoch_cached=2201`、`actual_ext4_fstat=3347`、
`epoch_miss=3934`。该统计只能证明新路径被走到，不能与旧 `lookup_directory_stat` 的次数直接相减。

### P10 准备

先固定 P9 并在同一 RISC-V `8G/8 hart` BuildStorm 配置下再取得一个十分钟样本，按 Cargo 标记而非 timeout 终点复核。
若 read-data/find 仍为首要 gate 等待，才增加每次实际底层 `inode.read_at()` 的来源聚合：mmap cache fill、regular-read
cold run、direct bypass、other，以及各自 bytes；来源总量需和 data-lock samples 交叉检查。统计只在 perf feature 下运行，
不对每个 page-cache hit 记账，不改变 lwext4 的单一串行锁或现有缓存/失效语义。归因前不修改预读长度、缓存上限或
正 dentry 路径。

## 2026-07-30：`tmp_01.ans` 复核 P9 并实现 backing-read 来源聚合

### 第二份 P9 窗口

`tmp_01` 含 P9 的 `file_cache_capacity`、目录 epoch fstat 标签和 `BUILDSTORM_TOOLCHAIN/MINIBUILD ok`；未匹配到
`panic/TFAIL/TBROK/ERROR`。最终完整 perf 快照为 `t=552564ms`，其后仅继续到 Cargo `Building 24/446`，没有
`BUILDSTORM_COMPILE`、END 或 `shutdown!`，故不作为完整 BuildStorm 或端到端性能验收。

该快照的 `read_bypass_file=37 ops / 30784 B`，`resident_pages=53470 / 98304`，且
`capacity_bypass_pages=0`。它独立复现了 `tmp_20` 的低旁路、缓存未触顶趋势（后者末尾分别为
`45 / 37440 B`、`60294 / 98304`、`0`），足以固定 P9 参数；两者末尾的 Cargo 依赖和锁交错不相同，不能从
`tmp_01` 的 read-data `349694294/30058520 us`、find `327209989/69373084 us` 与旧累计值得出吞吐比例。

### P10 实现

在 `os/src/utils/perf/fs.rs` 增加四个 `InodeReadSourceStats`（ops/bytes）和 `InodeReadSource`：
`MmapCacheFill`、`PageCachedReadColdRun`、`DirectBypass`、`Other`。报告增加：

```text
[perf] inode_read_source mmap_cache_fill_ops=... mmap_cache_fill_bytes=... \
       page_cached_cold_run_ops=... page_cached_cold_run_bytes=... \
       direct_bypass_ops=... direct_bypass_bytes=... other_ops=... other_bytes=...
```

接入点只包围真实、成功返回的 `Inode::read_at()`：`FilePageCache::get_or_load()` 的 `Mmap` 归入 mmap fill、
其 `Read` 归入 page-cached cold run、`Splice` 归入 other；`OSFile::try_page_cached_read()` 的多页冷 run 同样归入
page-cached cold run；只有该函数返回 `None` 后的 `OSFile::read()` 直读归入 direct bypass。缓存命中、EOF 前的
`inode.size()`、失败返回与 `read_all()` 都不增加该计数，避免把每个 page hit 的额外 atomic 加到热路径。

此行记录的是 VFS caller 对实际 `read_at()` 的需求，不是 device I/O：Ext4 delayed byte-cache 命中仍会有一次
VFS `read_at`，但不取得 `ext4_read_data_lock`。因此下一份样本以各来源 ops 与 `ext4 reads` 的同量级关系、以及
read-data lock samples 不超过相应 EXT4 读取工作量作合理性检查，而不要求两者严格相等。

### 验证计划

已通过 `cargo fmt --manifest-path os/Cargo.toml --all -- --check`、`git diff --check`、RISC-V/LoongArch64
`make perf` 和默认 `make TARGET_ARCH=riscv64`（其 release 子目标覆盖两种架构）；最后重建 RISC-V perf 内核。
构建只出现已有 Cargo config、vendored `smoltcp` 与 release `ipi_sent` warning。运行时使用 RISC-V `8G/8 hart` 的同一
BuildStorm 入口，确认 P9 标签与 `inode_read_source` 同时存在。按共同 Cargo 检查点保留来源 ops/bytes、`ext4 reads`、byte-cache hit、
read-data/find lock、read-bypass 和 resident/capacity-bypass；样本没有 `BUILDSTORM_COMPILE`、END、`shutdown!` 时
仍不宣称端到端加速。P10 未移动 lwext4 单一 gate，未修改预读、页缓存容量/准入或写入失效语义。

## 2026-07-30：`tmp_02.ans` P10 来源验证与未对齐 ELF 分块读取

### P10 运行期结果

`tmp_02` 同时输出 P9 标签和 `[perf] inode_read_source`，有 `BUILDSTORM_TOOLCHAIN/MINIBUILD ok`，没有
`panic/TFAIL/TBROK/ERROR`。最后完整快照为 `t=585973ms`，后续只到 Cargo `Building 23/446`，没有 compile/END/shutdown，
因而不作为完整功能或端到端性能结论。

末尾 `ext4 reads=32954 / 296203960 B`；mmap fill 为 `19486 / 183063432 B`，page-cached cold run 为
`9331 / 98809115 B`，direct bypass 为 `41 / 34112 B`，other 为零。前三桶总计 `28858 / 281906659 B`，与 ext4
总量相差 `4096 / 14297301 B`。同一快照 `ext4_read_data_lock samples=32072`，`byte_cache_read_hits=882`，满足
`32072 + 882 = 32954`；故来源缺口是真实成功 `read_at()` 调用，而非字节缓存或锁统计口径造成。

全局 `read_at` 审计定位四个漏记的 exec/ELF 点：`sys_execve` 的 256 B probe、ELF metadata 的初读/扩展，以及未对齐
`PT_LOAD` 的逐页装载。前两类每次 exec 只读小 metadata；`push_elf_segment_from_file()` 则会为未对齐段的每个目标页进入
lwext4，故是 4096 次、14.3 MiB 的主要候选。P10 的 mmap/page-cache 结论保持不变：mmap fill 已占约 59.1% reads，不能把
来源差额误解为继续扩大 cache 或预读的依据。

### P11 实现

为全部四个遗漏位置补记 `InodeReadSource::Other`，使 BuildStorm 的来源桶完整覆盖 VFS `read_at`。
`push_elf_segment_from_file()` 不再每页调用一次：它在已经 `map()` 的零初始化段中使用一个最大 64 KiB 的
`try_reserve_exact` 临时 buffer 完成一次连续 `read_at()`，再按原来的 `(data_offset, vpn, page_offset)` 规则分段复制。
每段文件数据不足一块时只分配所需容量；分配失败、错误、返回零或超过请求长度仍返回 `Err(())`，非零短读则与原来一样继续
读取剩余范围，段外字节未写入，继续保持原来的零填充。文件页缓存、ELF 段映射范围/权限、COW、auxv 和 lwext4 gate 均未调整。

64 KiB 上界将每个未对齐段的读取限制为 `ceil(segment_file_size / 64 KiB)` 次，代替原先最多按 4 KiB 目标页进入的多次
读取；`tmp_02` 的 14,297,301 B 还混有 metadata，不能仅按总字节数承诺全局调用次数。此为锁进入次数的可验证减少，
不是已实测的 BuildStorm 吞吐承诺。

### 验证与下一次采样

已通过 os fmt check、`git diff --check`、RISC-V/LoongArch64 `make perf` 和默认双架构 release 构建，最后恢复
RISC-V perf `kernel-rv`。只有已有 Cargo config、vendored `smoltcp`、release `ipi_sent` warning；没有运行 QEMU，以免
删除维护者 `disk.img` 链接。

下一次 RISC-V `8G/8 hart` BuildStorm 必须同时打印 P9/P10 标签。以相同 Cargo 标记检查：四个来源 bucket 的 ops/bytes
与 `ext4 reads` 的覆盖关系，`other` 的操作数和字节数，read-data lock samples/wait/hold，以及
`exec_loader_duration map_elf`。若 `other` 未降或总数不再与 reads 对齐，先修正分类；若其按预期降低而 mmap fill/cold run
成为主因，再为它们选择下一步而不先改缓存参数。

## 2026-07-30：`tmp_03.ans` 验证 P11 来源守恒并准备 P12

### 运行期证据

`tmp_03.ans` 已包含 P9 的 `file_cache_capacity`、目录 epoch fstat 标签以及 P10/P11 的
`inode_read_source`。日志有 `BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok`，未见
`panic`、`TFAIL`、`TBROK` 或 `ERROR`；最后快照约为 `t=599250ms`、Cargo `Building 26/446`。
由于没有 `BUILDSTORM_COMPILE`、END 或 `shutdown!`，该样本仍是中途路径归因窗口，不能作为完整回归或
端到端加速结论。

末尾计数如下：

```text
ext4 reads=29060 bytes=287043303 byte_cache_read_hits=645 byte_cache_read_hit_bytes=6657614
inode_read_source
  mmap_cache_fill       18869 ops / 173813719 bytes
  page_cached_cold_run   9335 ops /  98840795 bytes
  direct_bypass             44 ops /      36608 bytes
  other                    812 ops /  14352181 bytes
```

四类来源的 ops 和 bytes 分别精确相加为 `29060` 和 `287043303`，与 `ext4 reads` 完全一致。相较
`tmp_02` 中约 `4096 ops / 14.3 MiB` 的未归因缺口，P11 已将 exec probe、ELF metadata 和未对齐
`PT_LOAD` 段路径纳入 `other`，同时将未对齐段从逐页读改为最大 64 KiB 的有界分块读。`other=812`
仍包含 metadata 和其他非 mmap/page-cache caller，不能把它直接视为重复数据读取；不同 Cargo 检查点也不能用
总 reads 或锁累计值推导吞吐变化。

### P12 取证计划

当前 mmap fill 约占来源 `65%` 的 ops、`61%` 的 bytes，page-cached cold run 约占 `32%` 的 ops、`34%` 的
bytes，是下一步唯一有足够规模的候选。先在 `FilePageCacheSource::Mmap` 内把成功 backing read 分为 demand
page fault 与 `prefetch_shared_file_pages()` 的 fork/shared-map 预取；分类仍只在真实 `inode.read_at()` 成功后
做一次 `Relaxed` 累加，不在 cache hit 或 size 检查路径增加计数。

下一份同配置 RISC-V `8G/8 hart` 样本需并列保留 mmap demand/prefetch、page-cached cold run、
`ext4_read_data_lock` samples/wait/hold、page fault/readahead、resident/capacity-bypass。若 demand fill 占主导，
再检查 `prepare_file_page()` 与 `mmap_read_page_fault()` 是否重复触发；若共享映射预取占主导，则检查预取是否
覆盖已驻留页。完成来源比例和完整结束标记之前，维持 32 MiB/96K 页 P9 参数、四页预读上限及 EXT4 单一 gate。

## 2026-07-30：`tmp_04.ans` P12 来源细分实现

### 运行期基线

`tmp_04.ans` 仍装载 P11/RISC-V perf 内核，末尾完整快照约为 `t=577239ms`、Cargo `Building 26/446`；有
`BUILDSTORM_TOOLCHAIN/MINIBUILD ok`，未见 `panic/TFAIL/TBROK/ERROR`，但没有 `BUILDSTORM_COMPILE`、END 或
`shutdown!`。该快照的来源为 mmap fill `19861/187563419 B`、page-cached cold run `8660/92375936 B`、
direct bypass `42/34944 B`、other `791/14311446 B`，resident `57402/98304` 页且容量旁路为零。

### P12 实现

`FilePageCacheSource::Mmap` 拆为 `MmapDemand` 与 `MmapPrefetch`：`MemorySet::prepare_file_page()` 的真实缺页走
demand，`prefetch_shared_file_pages()` 的共享映射 fork 预取走 prefetch。`inode_read_source` 保留原
`mmap_cache_fill` 聚合并新增 demand/prefetch 两组 ops/bytes；只有实际 `inode.read_at()` 成功返回才累加，缓存命中、
`inode.size()` 与失效路径不增加诊断开销。页缓存容量、四页预读、EXT4 单一 gate 及失效语义均未改变。

### 验证计划

已运行格式化，下一步执行 `cargo fmt --manifest-path os/Cargo.toml --all -- --check`、双架构 `make perf` 与
`git diff --check`，随后用包含新字段的 RISC-V 样本判断 demand 与 prefetch 占比；未取得新样本前不宣称实际吞吐改善。

## 2026-07-30：`tmp_05.ans` demand mmap 归因与 EOF 重复加载优化

### 运行期证据

`tmp_05.ans` 已装载 P12/RISC-V perf 内核，末尾约 `t=588233ms`、Cargo `Building 36/446`；有
`BUILDSTORM_TOOLCHAIN/MINIBUILD ok`，未见 `panic/TFAIL/TBROK/ERROR`，但没有 `BUILDSTORM_COMPILE`、END 或
`shutdown!`。末尾 `mmap_demand=21328/206960356 B`、`mmap_prefetch=0/0`，`file_cache_source mmap_miss=21329`，
`resident_pages=64836/98304`、capacity bypass 为零，说明该窗口的 mmap 回源全部来自真实 demand fault，不能以预取
比例支持扩大预取深度。

### P13 修复

trap 的文件映射缺页此前先调用 `mmap_file_page_beyond_eof()`，该函数通过 `prepare_file_page()` 完整加载/查找页；
对未越过 EOF 的 fault，随后 `handle_page_fault()` 又执行一次相同页缓存查找。现在 EOF 检查只根据
`mmap_file_page_info()` 的页偏移和 `inode.size()` 判断完整越界页，避免第一次 `get_or_load()`；真正缺页仍由
`prepare_file_page()` 唯一加载；如果加载竞态期间截断使处理失败，trap 仅在失败冷路径重新检查 EOF 并区分 SIGBUS。页部分覆盖 EOF、SIGBUS、MAP_PRIVATE/MAP_SHARED 与 COW 语义保持不变。

### 验证计划

需以同一 RISC-V BuildStorm 入口复跑，比较 `file_cache hit/mmap_hit`、`mmap_miss`、`page_faults`、
`ext4_read_data_lock` 和尾页 SIGBUS 回归；样本无完整结束标记时不报告端到端加速。

## 2026-07-30：`tmp_06.ans` 验证 P12/P13 运行边界

### 运行期证据

`tmp_06.ans` 装载了包含 P12/P13 标签的 RISC-V perf 内核，最后快照为 `t=560923ms`、Cargo `Building 24/446`。
日志有 `BUILDSTORM_MINIBUILD ok`，未见 `panic`、`TFAIL`、`TBROK` 或 `ERROR`；但没有
`BUILDSTORM_COMPILE`、`#### OS COMP TEST GROUP END buildstorm ####` 或 `shutdown!`，所以不能把它当作完整
BuildStorm 功能回归或端到端性能样本。

快照计数为 `ext4 reads=29210 / 289132560 B`；来源为 mmap demand `19059 / 176501187 B`、mmap prefetch
`0 / 0 B`、page-cached cold run `9310 / 98278502 B`、direct bypass `43 / 35776 B`、other
`798 / 14317095 B`。四类来源的 ops/bytes 均精确覆盖 `ext4 reads`，确认 P12 的来源分类没有丢失调用点；
`mmap_prefetch=0` 说明本窗口没有 shared-map fork 预取回源。页缓存 `55956/98304` 页，
`capacity_bypass_pages=0`，P9 容量上限未触发。

### 结论与后续

该样本支持“mmap 回源仍以 demand fault 为主、来源归因守恒、缓存没有触顶”的取证结论，不支持端到端加速百分比。
由于 `tmp_05` 与 `tmp_06` 的 Cargo 阶段和并发交错不同，不能直接比较累计 `file_cache`、锁等待或 wall-clock。下一次
应在相同 Cargo 检查点比较 `file_cache hit/mmap_hit`、`mmap_miss`、`page_faults`、`readahead` 和
`ext4_read_data_lock`，并补做文件映射部分尾页、越界页及并发 truncate 的 SIGBUS 回归；在完整结束标记出现前保持
P9 的 32 MiB 准入、96K 页容量、四页预读、EXT4 单一 gate 及写入失效语义。
