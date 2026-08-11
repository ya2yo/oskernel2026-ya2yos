= lwext4 与磁盘文件系统引擎

== 概述与定位

`crates/lwext4_rust` 是 Ya2yOS 的磁盘文件系统引擎，由 C 实现的上游 lwext4 库与 Rust 绑定层组成。上游 lwext4 是一个面向微控制器的 ext2/ext3/ext4 文件系统库（BSD 类许可证，当前 fork 采用 GPL-2.0），这里在其上叠加了面向 SMP 内核的任务感知锁、块缓存并发与写回缓存等大量定制。第八章的 `os/src/fs/ext4_lw/` 通过 `lwext4_rust` 暴露的接口把 VFS 操作落到磁盘格式。

crate 的模块结构如下（`lib.rs`）：

```rust
mod ulibc;
pub mod bindings;
pub mod blockdev;
pub mod file;
pub mod perf;

pub use blockdev::*;
pub use file::{Ext4File, InodeTypes};
```

- `bindings.rs`：由 `c/wrapper.h` 经 bindgen 生成的 C 结构、常量和函数绑定；
- `blockdev.rs`：`Ext4BlockWrapper` 与 `KernelDevOp` trait，封装 `ext4_blockdev` 与块缓存，处理挂载/日志生命周期；
- `file.rs`：`Ext4File`，封装 C 侧文件描述符，并提供整文件写回缓存与稀疏写缓冲；
- `perf.rs`：可选性能遥测，聚合块缓存与写回缓存计数器；
- `ulibc.rs`：为无标准库的 C 代码提供 `printf`、`malloc` 等弱符号。

Cargo feature 控制三组能力：`print` 把 C 的 `printf` 路由到 Rust 的 `printf-compat` 兼容实现；`perf` 同时开启 C 侧 `EXT4_PERF_TELEMETRY` 与 Rust 侧计数器；`bcache-dirty-capacity-experiment` 开启脏块缓存高低水位回收实验。

== 构建与绑定

C 静态库由 `build.rs` 驱动 CMake 按目标架构编译。上游源码以子模块形式位于 `c/lwext4/`，构建时应用 `c/lwext4-make.patch` 并注入 crate 自带的 `musl-generic.cmake` 工具链：

- 按 `CARGO_CFG_TARGET_ARCH` 使用 `<arch>-linux-musl-*` 工具链，`-ffreestanding -fPIC -fno-builtin`，静态归档命名为 `liblwext4-{arch}-p212b2-default-{no-perf-}c2048.a`；
- 归档名编码 feature 变体与 `LWEXT4_BLOCK_CACHE_SIZE=2048`，并比较 C 源码与归档 mtime 决定是否重建，避免陈旧 Docker 产物被误复用；
- CMake 选项包含 `LWEXT4_USE_USER_MALLOC`（走 Rust 分配器）、`EXT4_PERF_TELEMETRY` 与 `EXT4_BCACHE_DIRTY_CAPACITY_EXPERIMENT`；
- x86_64 使用仓库提交的 `bindings.rs`，其他架构由 bindgen 基于 musl sysroot 重新生成。

由于 crate 是 `#![no_std]`，C 代码依赖的 libc 函数由 `ulibc.rs` 与 `c/ulibc.c` 提供弱符号回退：`printf` 走 `printf-compat`，`malloc/calloc/realloc/free` 通过带魔数和大小校验的 `MemoryControlBlock` 头交给 Rust `alloc`，并补充 `strcpy/strcmp/memset/qsort_r` 等。

== 磁盘布局模型

C 层的磁盘格式处理遵循标准 ext4 布局，结构全部 `#pragma pack(1)` 并手工小端转换：

- *superblock*（`struct ext4_sblock`）：完整 ext4 超级块，含 64 位块计数、日志 inode 号与 UUID、哈希种子、crc32c 校验和；
- *块组描述符*（`struct ext4_bgroup`）：块/ inode 位图地址、inode 表首块、空闲计数、`BLOCK_UNINIT/INODE_UNINIT` 标志与校验和，支持 64 位描述符；
- *inode*（`struct ext4_inode`）：模式、uid/gid、64 位大小、带 nsec/epoch 的时间戳、`blocks[15]`（12 个直接块 + 3 个间接指针，未用 extent 时作为 extent 根）、`file_acl`（xattr 块指针）与额外 isize；
- *目录*：线性目录项 `ext4_dir_en` 与 htree 结构（根/索引/叶节点）；
- *xattr*：inode 内嵌体（ibody）与 `file_acl` 指向的外部块两种位置，带 hash 与 crc32c 校验。

以 inode 为例，bindgen 生成的 `ext4_inode` 表示如下（`blocks[15]` 在启用 extent 时保存 extent 根，否则保存 12 个直接块与 3 个间接指针）：

```rust
#[repr(C)]
pub struct ext4_inode {
    pub mode: u16,
    pub uid: u16,
    pub size_lo: u32,
    pub access_time: u32,
    pub modification_time: u32,
    pub gid: u16,
    pub links_count: u16,
    pub blocks_count_lo: u32,
    pub flags: u32,
    pub blocks: [u32; 15],      // extent 根，或 12 直接 + 3 间接指针
    pub generation: u32,
    pub file_acl_lo: u32,       // xattr 外部块
    pub size_hi: u32,
    pub extra_isize: u16,
    pub checksum_hi: u16,
    pub ctime_extra: u32,
    pub mtime_extra: u32,
    pub atime_extra: u32,
}
```

feature 集支持 `EXT4_FINCOM_FILETYPE | META_BG | EXTENTS | FLEX_BG | 64BIT` 与只读兼容的 `SPARSE_SUPER | METADATA_CSUM | LARGE_FILE | GDT_CSUM | DIR_NLINK | EXTRA_ISIZE | HUGE_FILE`。`CONFIG_JOURNALING_ENABLE`、`CONFIG_XATTR_ENABLE`、`CONFIG_EXTENTS_ENABLE` 均开启，块缓存大小由 CMake 覆盖为 2048。

== 块缓存 bcache

块缓存用两棵红黑树维护：按 LBA 排序的 `lba_root` 与按 LRU id 排序的 `lru_root`，外加一个脏块单链表。每个缓冲区用原子位标志描述状态：`BC_UPTODATE`、`BC_DIRTY`、`BC_FLUSH`、`BC_LOADING`、`BC_WRITEBACK`、`BC_EVICTING`、`BC_IO_ERROR`，所有标志操作走 `__atomic_*`。

读路径采用单 loader 语义：`ext4_block_get` 命中时直接返回；miss 时用 CAS 抢占 `BC_LOADING` 位，获胜者发起物理读（`ext4_blocks_get_direct`），并发调用者调用 `ext4_bcache_wait_while` 等待——该等待会分发到宿主内核的 wait/wake 钩子（见下文），否则退化为 CPU 松弛。I/O 失败设置 `BC_IO_ERROR`，迟到的等待者拿到 `EIO`。`ext4_bcache_retain` 在索引锁内安全增加引用计数，修复了上游 `refctr++` 的 SMP 竞态。

写回由 `ext4_block_cache_flush` 驱动：用 `ext4_bcache_claim_dirty` 认领脏块，把连续 LBA 合并成最多 32 个块的批量请求（`EXT4_BLOCK_CACHE_FLUSH_BATCH_MAX_BLOCKS`）。`ext4_block_cache_write_back` 是挂载级嵌套计数器，只在最外层（`on_off=0`）真正排水，排水过程锁定顺序固定为 `journal_lock → cache_flush_lock → cache_lock`。

单个缓存缓冲区的 bindgen 表示为：

```rust
#[repr(C)]
pub struct ext4_buf {
    pub flags: c_int,   // BC_* 原子位标志
    pub lba: u64,
    pub data: *mut u8,
    pub lru_id: u32,
    pub refctr: u32,    // 在 index_lock 内安全递增
    pub bc: *mut ext4_bcache,
    pub on_dirty_list: bool,
    pub end_write: Option<unsafe extern "C" fn(...)>, // 磁盘写完成回调
}
```

== 日志与事务

日志（JBD）使用 inode 8 作为日志 inode，维护一个循环日志：`jbd_journal` 保存日志边界与事务 id，`jbd_trans` 保存单事务的脏缓冲区队列、revoke 树与块记录。元数据块通过 `ext4_trans_set_block_dirty` 进入事务，同时持有 bcache 引用并安装 `end_write` 回调。

提交路径为：分配 trans_id，写 descriptor 块与带块标签的数据拷贝（magic 与 `JBD_MAGIC` 相同时做转义），写 revoke 块与 commit 块，然后进入检查点队列。检查点按每个缓冲区的写完成回调推进，`written_cnt == data_cnt` 时日志起点前移并释放事务；`jbd_journal_make_room` 始终保留一个日志槽位，避免日志写满时的自旋断言。

恢复由 `jbd_recover` 对日志做三遍扫描：`ACTION_SCAN` 定位日志起止事务，`ACTION_REVOKE` 从 revoke 块构造撤销树，`ACTION_RECOVER` 把日志块重放到文件系统位置（跳过被新 revoke 覆盖的块）。恢复完成后清除 `EXT4_FINCOM_RECOVER` 并在 `super_lock` 下重新扫描所有块组计算空闲计数。

事务支持递归：`ext4_trans_start/stop/abort` 用嵌套计数，只有最外层才创建/提交共享的 `curr_trans`，内层失败设置 abort pending 由外层边界统一中止，并用 `ext4_fs_rwlock_write_owned_by_current` 校验持有权。

== 文件操作流程

*挂载与卸载*：`lwext4_mount` 依次执行 `ext4_device_register → ext4_mount → ext4_recover → ext4_journal_start → ext4_cache_write_back(mp, true)`；`lwext4_umount` 逆序回收。块设备回调 `dev_open/dev_bread/dev_bwrite/dev_close` 把 `blk_id × ph_bsize` 换算成字节偏移，要求完整长度（短读写返回 `EIO`）。

*目录与路径*：`ext4_dir_find_entry/add_entry` 优先走 htree：哈希名字后在索引/叶节点二分，叶内线性扫描并处理哈希碰撞链；树损坏时清除 `INDEX` inode 标志并回退到跨块线性扫描/插入。哈希函数支持 Linux 的 Half-MD4、TEA 与 legacy 三种。

*数据路径*：`ext4_fread` 在 inode 读锁下逐块解析，快速符号链接直接读 inode 内嵌数据，物理连续的非零块合并为直接块读，hole 与 unwritten extent 零填充。`ext4_fwrite` 在写锁与事务内分配/追加块，批量数据用直接 IO 写入；payload 与尾零填充都绕过 bcache，避免脏缓存别名。稀疏写保留目标逻辑块超过 EOF 的部分。

*extent 树*：根在 inode 的 `blocks` 区域。`ext4_extent_get_blocks` 是读写共用的映射入口：返回覆盖块的已有 extent、写时把 unwritten 区间转为 initialized（直接写零填充后转换）、或按目标块分配新块并插入。插入支持叶内前后合并、节点分裂、树加深（`ext4_ext_grow_indepth`）与索引修正；截断删除支持区间移除、叶中间拆分与整树高度收缩。

*分配*：块分配 `ext4_balloc_alloc_block` 目标位优先，其次按 64 位窗口与组轮转，空闲计数在 `super_lock` 下更新；释放块时写 revoke 记录并 `ext4_bcache_invalidate_lba` 丢弃缓存的已释放块。inode 分配 `ext4_ialloc_alloc_inode` 从 `last_inode_bg_id` 起扫描各组，优先空闲空间多的组。块位图与 inode 位图在 `BLOCK_UNINIT/INODE_UNINIT` 时惰性初始化，均在事务内完成。

*rename/truncate*：rename 支持替换既有文件（`ext4_frename`），并已修复替换时旧文件的生命周期；truncate 通过 extent 树收缩与 inode 尺寸更新，稀疏扩展保持逻辑块语义。

== SMP 锁钩子

这是 Ya2yOS 对 C 层最核心的定制。上游 lwext4 用单个挂载级 `EXT4_OP_LOCK` 串行化所有路径操作，这里被替换为一组按资源分类的条纹读写锁：

- `namespace_lock`：路径名与命名空间变更；
- `inode_locks[257]` 与 `group_locks[257]`：按 `inode % 257` / `bgid % 257` 分片；
- `super_lock`：超级块计数；
- `journal_lock`：日志事务；
- `cache_lock` 与 `cache_flush_lock`：写回缓存模式状态与排水。

每个资源锁的 C 结构只保存三类标量状态：

```c
struct ext4_fs_rwlock {
    int state;             /* 资源锁状态 */
    uint32_t writer_depth; /* 无钩子回退时写者递归深度 */
    uint8_t kind;          /* 资源类别，供宿主内核分类 */
};
```

`struct ext4_fs_rwlock` 保存 `state / writer_depth / kind`，`kind` 供宿主内核分类锁（例如避免 timer 扫描死锁）。C 侧通过 `ext4_fs_rwlock_set_hooks(ctx, lock_hook, unlock_hook, write_owned_hook)` 暴露可安装钩子：安装后每个 lock/unlock 都分发给宿主内核，由 `os/src/fs/ext4_lw/` 的任务感知 `TaskRwLock` 把等待者 park 在真正需要的资源上；无钩子（独立/宿主编译）时回退到原子自旋读写锁，允许写者通过 `writer_depth` 递归。`ext4_fs_rwlock_write_owned_by_current` 供事务助手检查当前任务是否持有写侧。

分片锁的覆盖范围与资源粒度对应：`inode_locks[257]` 按 `inode % 257` 分片，使不同 inode 的元数据操作并行，只有共享 inode 表块的读者互斥；`group_locks[257]` 按 `bgid % 257` 分片，使不同块组的位图与描述符更新互不阻塞。`namespace_lock` 串行化路径名与命名空间变更；`super_lock` 保护超级块空闲计数；`journal_lock` 保护日志事务；`cache_lock` 与 `cache_flush_lock` 保护写回缓存模式状态与排水。事务与锁交互遵循递归语义：`ext4_trans_start/stop/abort` 用嵌套计数，只有最外层创建/提交共享的 `curr_trans`，内层失败设置 abort pending 由外层边界统一中止；写侧归属用 `ext4_fs_rwlock_write_owned_by_current` 校验。完整锁序为 `journal_lock → cache_flush_lock → cache_lock`——cache flush 回调可能调用 journal 回调，该顺序保证检查点与缓存的交互串行化；超级块计数始终在 `super_lock` 下更新。

块缓存同样暴露 `ext4_bcache_setup_sync(ctx, wait, wake)`：一个 hart 加载/写回缓冲时，其他 hart 通过 wait/wake 钩子睡眠而不是自旋；`index_lock` 只保护 LBA/LRU 树与引用计数，且绝不跨越分配、I/O 或回调持有。缓冲加载采用单 loader：`ext4_block_get` miss 时用 CAS 抢占 `BC_LOADING` 位，获胜者直接读盘，其余调用者在 `ext4_bcache_wait_while` 上等待；写回由 `BC_WRITEBACK` 位与认领/释放机制串行。

Rust 侧写回缓存条目使用 `VFileCacheLock`（宿主锁钩子优先、`spin::RwLock` 回退）加独立的 flush 锁：`write_back_cache_entry` 先取 flush 锁快照缓存，释放后再按路径 `O_RDWR` 写回，写回后用 `revision` 校验避免并发写者丢失脏数据；条目的锁在驱逐时由 release 钩子归还，避免为每个缓存路径泄漏一个调度器锁。

== Ya2yOS 定制与写回缓存

在 C 层之上，Rust 侧维护一套整文件写回缓存与稀疏写缓冲，服务于编译器等大量小文件、稀疏文件写入的工作负载。设备接口由 `KernelDevOp` trait 抽象，要求位置无关的请求，保证 SMP 下块回调可并发：

```rust
pub trait KernelDevOp {
    type DevType;
    fn device_size(dev: &Self::DevType) -> Result<u64, i32>;
    fn read_at(dev: &Self::DevType, offset: u64, buf: &mut [u8]) -> Result<usize, i32>;
    fn write_at(dev: &Self::DevType, offset: u64, buf: &[u8]) -> Result<usize, i32>;
    fn flush(dev: &Self::DevType) -> Result<usize, i32>;
}
```

在此基础上维护以下状态：

- 写回缓存 `CACHE_TABLE` 按路径保存 `Arc<VFileCacheLock>`，`FIFO_TABLE` 是 32 项有界 LRU 写回队列；`write_back_cache_entry` 快照缓存、以 `O_RDWR` 重开路径写回，并用 `revision` 校验避免并发写者丢失脏数据；条目上限 4 MiB，对应 32 MiB 日志的提交空间；
- 稀疏写缓冲以 `(mount, inode)` 为键保存有界 range 集合（512 KiB / 32 段 / 8 MiB 全局预算），相邻与重叠写入先合并再提交，避免为每个子页写入展开 hole；
- `SEEK_DATA/SEEK_HOLE` 通过定制的 `ext4_fseek_data/hole` 实现；xattr 由 `99947f41` 补齐到 syscall 路径；
- 性能遥测分两层：C 侧 `BcachePerfStats`（命中/miss、loader、wait/wake、驱逐、写回、读写提交等约 34 项原子计数）与 Rust 侧 `WriteBackCachePerfStats`（缓存命中、直接写分类、稀疏写 flush 归因、rename/fstat 分阶段等约 60 项）；crate 只维护 relaxed 计数器，由内核 `os::utils::perf` 读取快照并补充锁与阶段计时。

== 当前边界

1. *日志空间约束*：整文件写回缓存条目上限按 32 MiB 日志空间设定；密集写回需为 descriptor/revoke 块与并行编译任务的检查点事务预留空间。
2. *块缓存容量*：`LWEXT4_BLOCK_CACHE_SIZE=2048` 是当前镜像的固定配置；脏块高低水位回收（256/128）由 `bcache-dirty-capacity-experiment` feature 开启，默认构建不启用。
3. *挂载模型*：当前为单挂载点/单设备模型（`ext4_fs` 挂在 `/`），未实现多 superblock 或挂载 namespace。
4. *一致性策略*：payload 数据与尾零填充走直接 IO 绕过 bcache，依赖 C 侧单 loader 与写回排水保证与缓存元数据的一致性；脏块别名依赖 `invalidate_lba` 与 revoke 清理。
5. *C 代码可移植性*：部分定制（条纹锁、直接 IO、遥测）是 Ya2yOS 专属，独立/宿主编译会退化为无钩子自旋路径，磁盘格式处理与上游保持一致。

#text(size: 8.5pt, fill: rgb("536471"))[_实现追溯：_ 本章对应 `crates/lwext4_rust/`（`src/{lib,blockdev,file,perf,ulibc,bindings}.rs`、`build.rs`、`c/lwext4/src/`）以及 `os/src/fs/ext4_lw/` 的当前实现。]
