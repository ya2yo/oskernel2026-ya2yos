= 文件系统

== 概述

Ya2yOS 的文件系统以 VFS 为统一抽象层，将系统调用、普通文件、设备文件、管道、套接字、epoll/eventfd/inotify、信号与消息队列等对象统一纳入文件描述符模型。底层磁盘文件系统采用 `lwext4_rust` 绑定的 lwext4，在 VirtIO 块设备之上提供 ext4 读写能力。

当前文件系统的主要组成如下：

- `vfs.rs` 定义 `SuperBlock`、`Inode`、`File` 三类核心 trait；
- `ext4_lw/` 将 lwext4 封装为 `Ext4Inode` 与全局 superblock，并为 C 侧资源锁安装任务感知的睡眠等待；
- `dcache.rs` 提供 VFS 层目录项缓存，`page_cache.rs` 提供普通文件读、mmap 与 splice 共享的页级缓存；
- `kernel_fs_ops/` 实现 `open`、`FsIndex` inode 缓存、初始目录/文件创建、proc 文件刷新和动态链接路径映射；
- `files/` 存放各种 `File` 实现，包括普通文件、管道/FIFO、设备文件、loop 设备、epoll、eventfd、inotify、fanotify、signalfd、mqueue、io_uring 占位、挂载上下文 fd 等；
- `fstruct.rs` 管理进程级文件描述符表；
- `fs_info.rs` 维护进程的当前目录、可执行文件路径、fd 到路径映射和 `umask`；
- `mount.rs` 维护路径化的挂载层、传播关系和 event group，并同步 `/proc/mounts`。

== VFS 抽象

=== SuperBlock

`SuperBlock` 表示一个文件系统实例的入口。当前实现主要由 ext4 superblock 提供：

```rust
pub trait SuperBlock: Send + Sync {
    fn root_inode(&self) -> Arc<dyn Inode>;
    fn sync(&self);
    fn fs_stat(&self) -> Statfs;
    fn ls(&self);
}
```

系统通过 `superblock_root_inode()` 获取根 inode，通过 `superblock_fs_stat()` 服务 `statfs/fstatfs`，通过 `superblock_sync()` 将缓存写回磁盘。

=== Inode

`Inode` 表示目录树中的文件系统节点。它负责路径查找、目录项创建、普通读写、元数据、链接与扩展属性操作：

```rust
pub trait Inode: Send + Sync {
    fn size(&self) -> usize;
    fn types(&self) -> InodeType;
    fn fstat(&self) -> Kstat;
    fn create(&self, path: &str, ty: InodeType) -> Result<Arc<dyn Inode>, SysErrNo>;
    fn create_with_metadata(&self, path: &str, ty: InodeType, mode: u32,
                            owner: Option<(u32, u32)>)
        -> Result<Arc<dyn Inode>, SysErrNo>;
    fn find(&self, path: &str, flags: OpenFlags, loop_times: usize)
        -> Result<Arc<dyn Inode>, SysErrNo>;
    fn find_from_cached_parent(&self, path: &str, flags: OpenFlags)
        -> Result<Arc<dyn Inode>, SysErrNo>;
    fn read_at(&self, off: usize, buf: &mut [u8]) -> SyscallRet;
    fn write_at(&self, off: usize, buf: &[u8]) -> SyscallRet;
    fn read_dentry(&self, off: usize, len: usize) -> Result<(Vec<u8>, isize), SysErrNo>;
    fn is_dir_empty(&self) -> Result<bool, SysErrNo>;
    fn truncate(&self, size: usize) -> SyscallRet;
    fn set_timestamps(&self, atime: Option<u64>, mtime: Option<u64>,
                      ctime: Option<u64>) -> SyscallRet;
    fn set_xattr(&self, name: &[u8], value: &[u8], flags: u32) -> SyscallRet;
    fn get_xattr(&self, name: &[u8], value: &mut [u8]) -> SyscallRet;
    fn link_cnt(&self) -> SyscallRet;
    fn unlink(&self, path: &str) -> SyscallRet;
    fn read_link(&self, buf: &mut [u8], bufsize: usize) -> SyscallRet;
    fn sym_link(&self, target: &str, path: &str) -> SyscallRet;
    fn rename(&self, path: &str, new_path: &str) -> SyscallRet;
    fn hard_link(&self, old_path: &str, new_path: &str) -> SyscallRet;
    fn read_all(&self) -> Result<Vec<u8>, SysErrNo>;
    fn path(&self) -> String;
    fn page_cache_path(&self) -> Option<Arc<str>>;
    fn cache_identity(&self) -> Option<(usize, usize)>;
    fn fmode(&self) -> Result<u32, SysErrNo>;
    fn fmode_set(&self, mode: u32) -> SyscallRet;
}
```

与早期版本相比，trait 新增了几类接口：

- `create_with_metadata()` 把 create/mode/owner 合并到一个临界区内，避免分开更新时被并发 open 观察到中间状态；
- `find_from_cached_parent()` 让已缓存父目录只做末级目录项查找，跳过高成本的全路径遍历；
- `is_dir_empty()` 供 `rmdir/unlinkat(AT_REMOVEDIR)` 在删除前保持 Linux 语义，防止后端递归删除；
- `set_xattr/get_xattr/list_xattr/remove_xattr` 默认返回 `EOPNOTSUPP`，由 ext4 覆写；
- `mark_directory_stat_changed()`、`touch_atime()` 支撑 stat 缓存的按目录失效和 atime 更新；
- `page_cache_path()` 返回共享的 `Arc<str>` 路径，供文件页缓存零拷贝构造键；`cache_identity()` 提供 `(st_dev, st_ino)` 身份键避免额外 `fstat()`。

目前真正落到磁盘的 inode 实现是 `Ext4Inode`。部分内核生成文件会复用 ext4 普通文件承载内容，而设备、管道、事件对象等不经过 `Inode`，直接实现 `File`。

=== File

`File` 是系统调用层面对 fd 操作的统一接口。普通文件、pipe、socket、epoll、eventfd、inotify、fanotify、signalfd、mqueue、设备文件都实现该 trait：

```rust
pub trait File: Send + Sync {
    fn update_signal_mask(&self, mask: SigSet) -> bool;
    fn readable(&self) -> bool;
    fn writable(&self) -> bool;
    fn read(&self, buf: UserBuffer) -> SyscallRet;
    fn write(&self, buf: UserBuffer) -> SyscallRet;
    fn truncate(&self, size: usize) -> SyscallRet;
    fn write_kernel_bytes(&self, buf: &[u8]) -> SyscallRet;
    fn fstat(&self) -> Kstat;
    fn path(&self) -> Cow<'_, str>;
    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet;
    fn nonblocking(&self) -> bool;
    fn set_nonblocking(&self, nonblocking: bool) -> SysResult;
    fn poll(&self, events: PollEvents) -> PollEvents;
    fn ioctl(&self, cmd: u32, arg: usize, memory_set: &MemorySet) -> SyscallRet;
    fn register(&self, context: &mut Context<'_>, events: PollEvents);
}
```

`poll()` 和 `register()` 让普通 fd 可以接入 `poll/ppoll/select/epoll` 的事件等待模型；`ioctl()` 默认返回 `ENOTTY`，由设备文件或特殊对象按需覆写。`update_signal_mask()` 用于 `signalfd4` 复用既有 fd 时更新信号掩码；`write_kernel_bytes()` 供内核内部向抽象文件注入数据。

== 文件描述符与进程文件上下文

=== FdTable

每个进程拥有一个 `FdTable`，内部通过读写锁保护：

```rust
pub struct FdTable {
    inner: RwLock<FdTableInner>,
}

pub struct FdTableInner {
    soft_limit: usize,
    hard_limit: usize,
    files: Vec<Option<FileDescriptor>>,
    reserved: Vec<bool>,
}

#[derive(Clone)]
pub struct FileDescriptor {
    flags: OpenFlags,
    file: FileClass,
}
```

默认 fd 表带有 `stdin/stdout/stderr` 三项，软上限为 128，硬上限为 256。`alloc_fd()` 使用最小可用编号策略，并在返回前把目标槽位标记为 `reserved`，避免并发线程同时分配到同一 fd；随后安装描述符时再清除预留。`dup`、`dup3`、`fcntl(F_DUPFD*)` 复制 `FileDescriptor`，共享底层 `Arc<File>`；`close_on_exec()` 会关闭带 `O_CLOEXEC` 的描述符。

`FileClass` 区分不同 fd 类型：

```rust
pub enum FileClass {
    File(Arc<OSFile>),
    Pipe(Arc<Pipe>),
    Socket(Arc<Socket>),
    FsContext(Arc<FsContextFd>),
    DetachedMount(Arc<DetachedMountFd>),
    Abs(Arc<dyn File>),
}
```

其中 `File` 用于普通 ext4 文件；`Pipe` 用于匿名管道与 FIFO 端点；`Socket` 用于网络套接字；`Abs` 用于设备、epoll、eventfd、inotify、signalfd、mqueue、io_uring 占位等抽象文件；`FsContext` 和 `DetachedMount` 支撑 Linux 新挂载 API 的 fd 流程。

=== FSInfo

`FSInfo` 保存进程文件系统环境：

```rust
pub struct FSInfo {
    inner: RwLock<FSInfoInner>,
}

struct FSInfoInner {
    cwd: String,
    exe: String,
    fd2path: HashMap<usize, String>,
    umask: u32,
}
```

- `cwd` 用于相对路径解析；
- `exe` 记录当前进程可执行文件路径；
- `fd2path` 辅助 `/proc`、`dup` 和调试场景维护 fd 到路径的映射；
- `umask` 默认 `0o022`，创建文件或目录时用 `mode & !umask` 计算最终权限。

== 路径解析与打开文件

`sys_openat()` 是用户态打开文件的主要入口。它从用户空间读取路径字符串，根据 `dirfd` 和进程 `cwd` 生成绝对路径，处理 `/proc/self/*` 等动态路径后调用 `open(abs_path, flags, mode)`；分配 fd 时写入 `FdTable` 并更新 `FSInfo.fd2path`。

`open()` 内部的查找采用分层缓存，尽量让热路径不进入 lwext4：

1. 先查 `FsIndex` inode 缓存（以 `(st_dev, st_ino)` 为身份键的路径索引）；
2. 未命中时，若父目录已缓存，通过 `find_from_cached_parent()` 只做末级目录项查找，并查询 `DENTRY_CACHE`；
3. 仍未命中时回退到根 inode 的完整路径 `find()`，结果回填到 inode 索引和目录项缓存。

查找结果会缓存最终 inode；`O_NOFOLLOW` 与内部 `O_UNLINK` 保留末级符号链接本身，且不会把符号链接写入普通路径缓存，避免后续普通 open 复用错误的链接节点。负目录项（确认不存在）也会缓存，服务非 `O_CREAT` 的探测路径；创建路径会先失效或绕过 negative 项。

打开流程还会检查路径所在挂载的标志：`NOSYMFOLLOW` 会为普通 open 追加 `O_NOFOLLOW`，`NODEV` 禁止打开设备节点，只读挂载上的 `O_CREAT` 返回 `EROFS`。`O_PATH` 创建仅路径描述符，忽略创建、截断和访问模式；`O_NOATIME` 需要文件所有者或 `CAP_FOWNER`；创建新节点时按父目录权限、进程 `umask` 计算最终权限，并在父目录带 `S_ISGID` 时继承组 ID 与 setgid 位。以写模式打开既有普通文件时，还会向持有文件租约的进程发送租约断开通知。

`/proc/uptime` 与 `/proc/<pid>/pagemap` 是动态 proc 节点：前者按需合成开机时长，后者在读取时按地址空间实时生成页表项，避免为每个 fork 的进程分配数百 MiB 的 ext4 空洞文件。

`O_TMPFILE` 目前是一个内存匿名普通文件（`files/tmp_file.rs`）：`openat` 只使用目标目录选择文件系统上下文，返回的 fd 持有内存中的数据和权限，直到关闭或通过 `linkat("/proc/self/fd/<fd>")` 落地为真实目录项，而不是在目录下创建 `N.tmp`。

== 目录项与 inode 缓存

`FsIndex` 是 VFS 层 inode 缓存，`DENTRY_CACHE` 是目录项缓存，二者分别加速“路径 → inode 对象”和“父目录 → 子 inode”的映射。

```rust
struct InodeCacheState {
    paths: HashMap<String, InodeCacheKey>,   // 绝对路径 -> 身份键
    inodes: HashMap<InodeCacheKey, Arc<dyn Inode>>, // 身份键 -> inode 对象
}

enum InodeCacheKey {
    Inode { dev: usize, ino: usize }, // 正常文件的 (st_dev, st_ino)
    Path(String),                     // 无 inode 号后端的回退键
}
```

路径索引与 inode 缓存必须在同一次锁持有中更新，否则底层 ext4 在两步之间复用 inode 号时，新文件可能错误拿到已删除文件的 `Arc<dyn Inode>`。缓存上限为 32K 个 inode；`SPECIAL_NODE_TYPES` 表为 FIFO、设备节点等无法从后端稳定取得类型的节点保留创建时类型。

目录项缓存以 `(父目录 inode 地址, 子项名)` 为键，避免为构造键再次调用 `path()` 或 `fstat()`。缓存项分 `Positive`（保存子 inode）和 `Negative`（确认不存在）两类；`create/link/unlink/symlink/rename` 等命名空间操作显式回填或失效条目。缓存有界，达到上限和进程回收边界时统一清理。

这套分层缓存显著降低了深层路径的打开开销：父目录缓存命中时，`open()` 直接复用 `FsIndex` 中的父 inode 与 `DENTRY_CACHE` 中的子项，不再为每个目录层级重复进入 lwext4；末级 miss 时所有父级已完成解析，child 不存在无需再逐前缀扫描中间符号链接。

== 文件页缓存

`os/src/fs/page_cache.rs` 为普通文件的 `read`、`mmap` 和 `splice` 路径提供共享的页级缓存。缓存以 `(文件路径, 页号)` 为键保存页帧，帧直接使用 mm 的 `Arc<FrameTracker>`，因此文件映射与缺页路径复用同一物理页：

```rust
pub struct FilePageKey {
    pub path: Arc<str>,      // 共享路径，零拷贝构造键
    pub page_index: usize,   // 文件内以 PAGE_SIZE 为单位的页号
}

pub struct FilePage {
    pub key: FilePageKey,
    pub frame: Arc<FrameTracker>,
    pub valid_len: usize,    // 尾页有效字节数
}
```

缓存容量由全局页数上限控制（默认 192K 页，约 768 MiB）。索引采用路径分组的两级结构，路径以 `Arc<str>` 跨 inode 别名共享，避免数十万次命中反复分配和复制 pathname。容量不足时使用带延迟队列的 CLOCK 策略回收未被引用的干净页：刚访问过的页、仍被其他对象引用的页和脏页不会被立即回收，固定页候选会在累计若干次容量失败后按小批次重试，避免 mmap 密集负载反复扫描整个工作集。顺序缺页最多预读 4 页（16 KiB），摊薄 lwext4 锁和单页读取开销；超过 32 MiB 的大文件只探测一次缓存准入，避免 mmap 触发重复的准入探测。

缓存只负责查找、加载、预读和失效，不负责把脏页写回文件；写入路径通过 `mark_dirty` 标记脏页，回写仍由 lwext4 与 `sync` 路径完成。文件映射的缺页通过 `FilePageCacheSource::MmapDemand` 与 `MmapPrefetch` 区分来源，命中后直接把缓存的 `FilePage` 帧映射到地址空间。

普通读路径与文件映射共用这些帧：大于一页的 `read()` 只对连续未命中的冷页段进入 `inode.read_at()`，命中页直接拼装到输出，避免单页 miss 导致整段回源重新读取；干净的只读私有文件页可跨进程复用同一缓存帧，通过 COW 保持私有写语义。

== ext4 与 lwext4 集成

=== 并发与资源锁

ext4 适配层早期使用挂载级的 `EXT4_OP_LOCK` 串行化所有文件系统操作。该锁竞争严重且会让无关操作互相等待；当前实现已把它替换为按资源分类的任务感知锁。C 侧 `struct ext4_fs_rwlock` 的地址即资源键，标识命名空间、inode 分片、块组分片、日志、超级块或缓存资源；Rust 侧为每个锁维护 FIFO 等待队列，发生竞争时任务睡眠在它实际需要的资源上，而不是在锁持有者被抢占时自旋。

```rust
struct TaskRwLockState {
    readers: BTreeMap<usize, TaskRwLockReader>, // 按任务记录读锁递归深度
    writer: Option<usize>,
    writer_depth: usize,
    next_ticket: usize,
    waiters: VecDeque<TaskRwLockWaiter>,        // FIFO 票号队列
}
```

锁模式按任务递归：同一任务可重入读锁（适配 readlink → fread 等 C 封装），写锁持有者可进入读区间；等待者按票号 FIFO 排队，已排队的写者之前新读者不能插队，避免读者饿死写者。任务退出时会通过 `cancel_ext4_op_waiter` 清理被抛弃的锁等待。两层之间的锁顺序固定为 `VFS write_state -> VFS io_state -> lwext4 resource locks`；C 层持锁时不回调 VFS，Rust 侧 dentry、inode 索引和页缓存锁持有时间很短，并在进入 lwext4 或发起块 I/O 前释放。块设备层（`os/src/drivers/disk.rs`）也采用任务感知的 FIFO 等待，减少 SMP 下块设备提交的竞争。

=== 块设备适配

Ya2yOS 通过 `lwext4_rust` 将 lwext4 接入 Rust 内核。适配层向 lwext4 提供块设备接口：

```rust
pub trait BlockDevice {
    fn read(&self, buf: &mut [u8]) -> Result<i32, SysErrNo>;
    fn write(&self, buf: &[u8]) -> Result<i32, SysErrNo>;
    fn seek(&self, pos: usize);
    fn size(&self) -> usize;
}
```

内核的 `Disk` 结构实现该接口，使 ext4 可以直接在 VirtIO 块设备上执行目录项、inode、数据块和元数据操作。块设备层为 lwext4 提供任务感知的 FIFO 提交，并接入 bcache 与块请求遥测。

=== Ext4Inode

`Ext4Inode` 是 `Ext4File` 的 VFS 封装，主要能力包括：

- `create()/create_with_metadata()`：创建普通文件或目录，并返回新的 VFS inode；
- `find()/find_from_cached_parent()`：检查目录、普通文件和符号链接，符号链接支持绝对/相对目标；
- `read_at()/write_at()`：打开底层 ext4 文件，seek 到指定偏移后读写；
- `read_all()`：一次性读取普通文件内容，符号链接会递归读取目标；
- `read_dentry()`：将 lwext4 目录项编码为 `getdents64` 可返回的数据；
- `truncate()`：通过 lwext4 截断或扩展文件，支持 `SEEK_DATA/SEEK_HOLE` 语义；
- `rename()/hard_link()/sym_link()/unlink()`：提供重命名、硬链接、软链接和删除；
- `fstat()/fmode()/fmode_set()/set_timestamps()`：返回和修改 Linux 兼容的元数据与时间戳；
- `set_xattr()/get_xattr()/list_xattr()/remove_xattr()`：提供真实扩展属性读写；
- `sync()`：刷新文件缓存；
- `delay()`：标记延迟删除，inode drop 时移除文件。

`Ext4Inode::fstat()` 会将 lwext4 返回的元数据转换为内核 `Kstat`，并对部分时间戳高位进行兼容性修正。`size()` 使用 `known_size` 快路径避免热路径重复进入 lwext4，`known_size` 在截断/写入等操作时失效；inode 类型（`types()`）走无锁只读字段，避免类型判断再次查询路径。

=== stat 与元数据缓存

目录的 `fstat`/`stat` 结果按目录本地元数据 epoch 缓存：一次 pathname lookup 取得的目录 stat 会被后续同目录查询复用，避免重复进入 lwext4。`mark_directory_stat_changed()` 钩子由命名空间操作（create/rename/rmdir 等）调用，推进受影响父目录的本地 epoch；`stat_cache_miss_reason` 记录每种 miss 的来源用于性能归因。缓存按目录局部失效，跨目录不互相干扰。

== 普通文件 IO

`OSFile` 封装一个普通 ext4 inode，并维护当前文件偏移：

```rust
pub struct OSFile {
    readable: bool,
    writable: bool,
    pub inode: Arc<dyn Inode>,
    inner: Mutex<OSFileInner>,
}

struct OSFileInner {
    offset: usize,
}
```

`read()` 从当前 offset 调用 `inode.read_at()`，读到 EOF 返回 0，并推进 offset。大于一页的读请求会先尝试拼装文件页缓存中已驻留的页，只有未缓存的连续段才进入 `inode.read_at()`，避免单页 miss 导致整段重新读取并争用 ext4 锁。`write()` 调用 `inode.write_at()` 写入每个用户缓冲片段并推进 offset。`lseek()` 支持 `SEEK_SET`、`SEEK_CUR`、`SEEK_END`，并支持 `SEEK_DATA/SEEK_HOLE`（在 lwext4 C 层实现）。`OSFile` 在 open 时缓存文件类型，`lseek()` 不再为每次调用重复查询路径与特殊节点表；FIFO/socket 的 `ESPIPE` 语义保持不变。

系统调用层为了避免超大用户请求导致内核堆 OOM，将 `read/write/readv/writev/pread64/pwrite64/sendfile/copy_file_range` 等操作按 64 KiB 上限分片或限制单次内核缓冲区大小。`fsync/fdatasync/sync_file_range` 最终调用 inode 的 `sync()`；当前不区分数据和元数据同步。

`sendfile()` 和 `copy_file_range()` 当前通过内核缓冲区在两个 fd 之间转发数据，并支持显式 offset 指针的读写回填，不是真正的零拷贝实现。

`fallocate()` 支持默认模式和 `FALLOC_FL_KEEP_SIZE`，会根据 `statfs` 的可用块数做空间检查；未支持的模式返回 `EOPNOTSUPP`。

=== 写路径与写回缓存

写路径围绕 lwext4 的整文件写回缓存做专门优化。同一路径已打开的 `O_RDWR` 描述符可直接服务只读访问，避免 `O_RDWR -> O_RDONLY -> O_RDWR` 重开。写入命中已缓存且位于配额预留范围内时，以每 inode 的可睡眠状态锁串行，不再占用全局 lwext4 gate；只有 cache miss、稀疏或超限写入、延迟删除、淘汰回写才进入 lwext4 慢路径。整文件缓存采用有界 LRU，写命中提升活跃产物，避免并行编译的临时文件反复被驱逐、重建和整文件回写。写入前通过挂载表 `reserve_write` 预留字节配额，失败回滚，预留/回滚逻辑移出全局操作锁。稀疏写入以 `(mount, inode)` 有界 range 缓冲记录已提交的稀疏范围，读取时按范围覆盖 hole；rename 拆分 byte-cache 写回与 mount-wide flush，不再隐式同步整个挂载。

== 目录项与元数据

`getdents64` 通过 `Inode::read_dentry(off, len)` 读取目录项。ext4 层从 lwext4 获取目录项列表，并按用户缓冲区容量逐项编码返回，同时返回新的目录偏移。

`Kstat` 与 Linux `stat` 结构兼容：

```rust
pub struct Kstat {
    pub st_dev: usize,
    pub st_ino: usize,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: usize,
    pub st_size: isize,
    pub st_blksize: i32,
    pub st_blocks: isize,
    pub st_atime: usize,
    pub st_atime_nsec: usize,
    pub st_mtime: usize,
    pub st_mtime_nsec: usize,
    pub st_ctime: usize,
    pub st_ctime_nsec: usize,
}
```

`Statfs` 返回文件系统块数、inode 数、文件名长度、挂载标志等统计信息。`statfs()` 当前直接返回全局 ext4 superblock 数据；`fstatfs()` 对 fd 做有效性检查后同样返回全局统计。

== 管道、FIFO 与 splice

=== Pipe

匿名管道由 `Pipe` 端点与共享 `PipeRingBuffer` 组成。缓冲区按字节容量限制保存 `PipeBuf` 片段队列，而不是早期版本的固定头尾环形数组：

```rust
pub struct Pipe {
    readable: bool,
    writable: bool,
    nonblocking: AtomicBool,
    buffer: Arc<Mutex<PipeRingBuffer>>,
}
```

```rust
pub(super) enum PipeBufStorage {
    Bytes(Arc<Vec<u8>>),     // 普通 write 产生的数据
    FilePage(Arc<FilePage>), // 来自文件页缓存的帧引用
}
```

普通 write 直接以用户缓冲片段生成 `Bytes` 片段，不再先聚合临时 `Vec`；大读把片段直接写入用户页片段，同样避免聚合中间 `Vec`。空管道读会睡眠等待写入或写端关闭，满管道写会等待读端释放空间，非阻塞模式返回 `EAGAIN`；无读端时写返回 `EPIPE` 并发送 `SIGPIPE`。等待者通过弱引用任务队列与 `PollSet` 唤醒，避免在管道压力测试中忙等。管道容量可通过 `F_SETPIPE_SZ` 调整，受 `CAP_SYS_RESOURCE` 与全局 sysctl 上限约束。

=== FIFO 命名管道

FIFO 使用路径键控的共享环形缓冲表：`open_fifo(path, flags)` 按路径查找或创建缓冲区，读端/写端共享同一 `PipeRingBuffer`，并支持 `O_NONBLOCK` 下无读端时写打开返回 `ENXIO`。FIFO 端点是普通 `Pipe` 的变体，读/写/等待语义与匿名管道一致。

=== splice

`splice` 为 pipe → pipe 提供零拷贝快路径：直接移动 `PipeBuf` 片段本身（`Bytes` 移动 `Arc<Vec<u8>>`，`FilePage` 移动 `Arc<FilePage>`），两个 pipe 之间不复制片段内的实际数据；`tee` 通过克隆片段元数据保留输入 pipe 的数据。同时持有两个 pipe 缓冲时按 `Arc` 地址排序加锁，避免 splice/tee 形成锁序死锁。文件与 pipe 之间的 splice 复用文件页缓存，将缓存帧作为 `FilePage` 片段推入管道。

== devfs、loop 设备与 proc 动态文件

=== devfs 与 loop 设备

`devfs` 维护一个设备路径到设备号的表，`open()` 识别设备路径后返回对应抽象文件。当前支持：

- `/dev/null`：读返回 EOF，写丢弃数据；
- `/dev/zero`：读返回全 0，写丢弃数据；
- `/dev/random`：提供伪随机/兼容性数据；
- `/dev/rtc`、`/dev/rtc0`、`/dev/misc/rtc`：返回简化 RTC 时间；
- `/dev/tty`：复用标准输入输出；
- `/dev/cpu_dma_latency`：记录 CPU 延迟配置；
- `/dev/loop-control`、`/dev/loopN`、`/dev/loop/N`、`/dev/block/loopN`：提供 loop 设备 ioctl 兼容。

loop 设备维护 256 项 `LoopState`，支持 `LOOP_CTL_GET_FREE`、`LOOP_SET_FD`、`LOOP_CLR_FD`、`LOOP_GET_STATUS*`、`LOOP_SET_STATUS*` 和 `BLKGETSIZE64` 等接口。当前 loop 块设备主要服务 LTP/BusyBox 兼容，尚未把数据读写真实转发到 backing file。

=== proc 动态文件

Ya2yOS 创建 `/proc`、`/proc/mounts`、`/proc/meminfo`、`/proc/sys/kernel/*` 等基础文件，并为每个进程创建 `/proc/<pid>/stat`、`/proc/<pid>/status`、`/proc/<pid>/maps`。这些文件的内容由内核生成后写入 ext4 文件；访问 `/proc/self/*` 或指定 pid 文件时会按需刷新。

`/proc/uptime` 是动态只读节点，按当前时钟与空闲 tick 实时合成；`/proc/<pid>/pagemap` 在读取时按地址空间逐页生成 `PFN/PRESENT` 位，避免为每个 fork 进程在 ext4 上分配与最高用户地址等大的稀疏文件。`/proc/<pid>/status` 提供 Uid/Gid 与真实进程组信息。

这不是完整的独立 procfs，而是一个以 ext4 文件承载内容、叠加动态节点的兼容实现，重点满足 libc、BusyBox 和 LTP 对常见 proc 节点的读取需求。

== IO 多路复用与事件 fd

=== poll/select/epoll

普通文件、管道、设备、socket、eventfd、inotify、signalfd 等对象通过 `File::poll()` 暴露就绪状态。`ppoll()` 和 `pselect6()` 线性扫描用户传入的 fd 集合；`epoll_create1()` 创建 `EpollFile`，`epoll_ctl()` 管理监听集合，`epoll_pwait()` 返回就绪事件或阻塞等待。

epoll 文件对象内部维护：

- `registry`：fd 到监听事件的映射，与进程本地 fd 复用隔离，避免 fd 编号复用时串扰；
- `ready_list`：已触发的事件队列；
- `PollSet`：用于唤醒等待中的任务。

近期修复包括唤醒丢失（写入事件与任务睡眠之间的竞态）、stdin 与 eventfd 的 poll 就绪报告，以及 fd 分配预留与并发分配竞态。

=== eventfd

`eventfd2(initval, flags)` 创建一个 64 位计数器 fd，支持 `EFD_CLOEXEC`、`EFD_NONBLOCK`、`EFD_SEMAPHORE`：

- 普通模式读返回计数器值并清零；
- 信号量模式读返回 1 并将计数器减 1；
- 写入会累加计数器，写入 `u64::MAX` 返回 `EINVAL`；
- 计数器为 0 时阻塞读，计数器接近溢出时阻塞写；
- `poll()` 在计数器大于 0 时报告可读，在未满时报告可写。

=== signalfd、inotify、mqueue 与相关对象

`signalfd4()` 从用户掩码构造 `SignalFd`，读取时返回任务的 pending 信号队列；掩码剥离 `SIGKILL/SIGSTOP`，支持 `SFD_CLOEXEC`/`SFD_NONBLOCK`，并可用 `update_signal_mask()` 修改既有 fd 的掩码。`inotify_init1()` 创建 inotify fd，`inotify_add_watch()` 和 `inotify_rm_watch()` 管理 watch descriptor；当前 watch 管理、事件队列、read/poll 框架已经具备，但文件系统变更事件的自动生成仍是后续工作。`fanotify` 提供 `fanotify_init` 的 fd 载体、`fanotify_mark` 的 mark 表和覆盖 LTP `fanotify01` 的基础事件队列；权限事件响应与完整传播语义仍需接入。

POSIX mqueue 通过全局名称表管理队列，`mq_open()` 创建或打开命名队列，`mq_timedsend()` 和 `mq_timedreceive()` 发送/接收消息，支持非阻塞语义。`mq_notify()` 当前返回 `ENOSYS`。

`memfd_create()` 返回匿名内存文件（`TmpFile`），校验 `MFD_*` 标志并支持 `MFD_CLOEXEC`。`io_uring_setup()` 返回独立的 `IoUringFd` 占位 fd，并提供真实的 SQ/CQ ring offsets 与 `IORING_MAX_ENTRIES` ABI 结构，但提交/完成环引擎尚未实现。`timerfd_create`、`memfd_secret`、`perf_event_open` 等仍返回 dummy fd 或 `ENOSYS`，用于避免用户程序在能力探测阶段直接失败。

== 挂载接口

`MNT_TABLE` 是当前挂载语义的状态中心。每个 `MountEntry` 保存
`(special, dir, fstype, flags)` 以及 `shared_group`、`master_group`、
`unbindable`、`event_group`。表最多容纳 256 个条目，允许同一路径出现多个挂载层；
按路径查询时选择最长覆盖路径，同一挂载点选择最新层，因此 `umount2()` 删除顶层后会
重新显露更早的层。`/proc/mounts` 由该表序列化，根 ext4 记录始终存在。

传统 `mount()` 支持普通挂载、remount、`MS_BIND`、`MS_MOVE` 和 propagation-only
调用。`MS_SHARED` 为选定挂载副本建立 peer group，`MS_SLAVE` 保留上游 master 关系，
`MS_PRIVATE`/`MS_UNBINDABLE` 断开传播关系；`MS_REC` 可将这些属性递归应用到子树。
在 shared 挂载或其 slave 后代下创建子挂载时，表会按相对路径扩展副本：事件从 peer
流向 slave，但不会由 slave 反向传播到 master。`MS_UNBINDABLE` 源不能被 bind clone，
bind 事件会经 peer/slave 传播，`MS_MOVE` 移动整棵子树并清除源挂载。

`MS_MOVE` 移动整棵挂载子树，并为目标父挂载可达的 peer/slave 创建副本；每次普通
挂载或移动产生一个 `event_group`，卸载任何一个副本时会同时删除同组副本。bind/move
的系统调用层还会在表锁外镜像目录树：目录递归创建，普通文件使用 hard link。这使当前
路径式 VFS 能观察到常见 bind/传播测试的目录形状，同时避免在递归 bind 中复制目标自身。

`fsopen`、`fsconfig`、`fsmount`、`fspick`、`open_tree`、`move_mount` 和
`mount_setattr` 已提供 fd 类型、参数校验与最小状态流。`fsconfig` 记录选项并在 create
后允许 `fsmount` 生成 detached mount fd；`move_mount` 可将它落入 `MNT_TABLE`。这些
接口尚未创建独立 superblock 或 namespace，`mount_setattr` 目前主要校验 ABI 结构。
fresh `tmpfs` 挂载会清空底层挂载点目录，以在该简化模型中近似空的 tmpfs 根目录。
跨进程仍持有打开的 inode 时，`unlink` 会延迟到底层引用释放后执行，避免删除后仍可
通过 fd 访问的路径被缓存污染。挂载表还为每个挂载维护模拟配额：`reserve_write` 在
写前预留字节配额，失败回滚，容量由 loop 设备格式化镜像大小推导。

== 文件系统锁设计

文件系统是当前内核中并发最密集的子系统之一。锁按 `VFS 层 → ext4 适配层 → lwext4 C 资源锁 → 块设备` 分层组织，设计目标是把竞争收敛到真正需要的资源粒度上，而不是让整个文件系统或单个挂载成为单一临界区。

=== VFS 层锁

VFS 对象锁保护各缓存与描述符的短期状态，锁持有时间都很短，且不跨越文件系统 I/O、调度或信号路径：

- `FsIndex` 与 `DENTRY_CACHE` 用 `RwLock` 保护路径索引与目录项缓存，路径到 inode 的映射在同一次锁持有中更新，避免 inode 号复用被观察到中间态；
- 文件页缓存 `FILE_PAGE_CACHE` 用 `RwLock` 保护两级索引，驱逐队列用独立 `Mutex` 保护；
- `OSFile` 的偏移与可变状态用 `Mutex`，管道共享缓冲用 `Arc<Mutex<PipeRingBuffer>>`；
- `FdTable`、`FSInfo` 分别用 `RwLock` 保护进程级描述符表与文件系统环境，`MNT_TABLE` 用 `Mutex` 保护挂载表。

这些锁在进入 lwext4 或发起块 I/O 前释放，因此不会与底层锁嵌套。

=== 任务感知锁

ext4 适配层把 C 侧锁钩子接到任务感知的 `TaskMutex`/`TaskRwLock`：发生竞争时调用者睡眠在它实际需要的资源上，而不是在锁持有者被抢占时自旋；没有当前任务（启动阶段）的调用者保留自旋回退。锁按任务递归——同一任务可重入读锁（适配 readlink → fread 等 C 封装），写锁持有者可进入读区间；等待者按票号 FIFO 排队，已排队的写者之前新读者不能插队，避免读者饿死写者。任务退出时通过 `cancel_ext4_op_waiter` 清理被抛弃的锁等待。C 侧 `struct ext4_fs_rwlock` 的地址即资源键，标识命名空间、inode 分片、块组分片、日志、超级块或缓存资源，具体分片与锁序详见第九章。

=== 锁序与内存衔接

两层之间的锁顺序固定为 `VFS write_state -> VFS io_state -> lwext4 resource locks`；C 侧资源锁之间为 `journal_lock -> cache_flush_lock -> cache_lock`，超级块计数始终在 `super_lock` 下更新。C 层持锁时不回调 VFS。块设备层（`os/src/drivers/disk.rs`）也用任务感知的 FIFO 提交队列：单一同步 VirtIO 队列要求请求独占通过对齐批量或未对齐 RMW，任务用 `block_on` 挂起而不是自旋，任务退出时 `cancel_disk_waiter` 清理票号。

与内存管理的衔接遵循既有约定：页表更新遵守 `UPDATE_LOCK -> MemorySet 写锁`，跨地址空间操作先快照 `Arc` 帧再切换锁；进入文件系统、调度、网络或信号路径前释放 `MemorySet` guard。文件缺页、fork 预取与共享页写回均做"锁外 I/O、锁内短暂页表更新"，避免把可睡眠的文件系统锁嵌套进内存锁，也避免在持有内存锁时进入 lwext4。

== 文件锁、fcntl 与 xattr

`fcntl()` 支持 fd 复制、`FD_CLOEXEC`、`O_NONBLOCK` 查询/设置、pipe 大小查询、文件 owner 相关兼容返回，以及 POSIX record lock / OFD lock 的基本语义。`F_SETLEASE/F_GETLEASE` 提供文件租约，`F_SETOWN/F_SETSIG` 等配置异步 I/O 信号目标。`fchdir` 与 `fchmodat2` 提供基于 fd 与 `AT_*` 标志的目录/权限操作。

文件锁实现分三类：

- `flock()`：按打开文件描述（而非路径）维护整文件 advisory lock，支持共享锁、排他锁、非阻塞失败和阻塞等待唤醒；
- `F_GETLK/F_SETLK/F_SETLKW`：按 inode 路径维护字节区间锁，`F_SETLKW` 在等待图中做死锁检测；OFD lock 按打开文件描述隔离，委托给同一套记录锁逻辑；
- `F_SETLEASE`：文件租约在冲突打开时通过 `SIGIO` 通知持有者，由 open 路径调用 `notify_file_lease_break()`。

扩展属性已有真实的 syscall 与文件系统路径：`setxattr/getxattr/listxattr/removexattr` 经 `os/src/syscall/fs/xattr.rs` 解析用户指针后调用 `Inode::*_xattr`，由 lwext4 在文件系统操作锁内完成 `XATTR_CREATE/XATTR_REPLACE` 校验与数据读写。

== 动态链接支持

`map_dynamic_link` 模块负责兼容用户程序期望的动态链接器和共享库路径。`open()` 和 `openat()` 会在进入 ext4 前做路径映射；`read_at()` 和 `read_all()` 会通过 `patch_dynamic_link_file_bytes()` 对部分动态链接文件内容做运行时修补。

这套机制让固定镜像中的 musl/glibc 程序可以在内核统一的文件系统布局上运行，同时避免把所有路径兼容都硬编码到用户态。链接器从原生 libc script 显式命名解释器时优先使用该解释器，镜像缺失时才回退到兼容目标路径。

== 当前边界与后续方向

1. *真实多文件系统挂载*：挂载表已支持叠加层、bind/move 子树和 shared/slave 传播，但路径解析仍没有挂载点 inode 切换、独立 superblock 或 mount namespace；bind 目录可见性依赖目录镜像而非 VFS dentry 切换。
2. *tmpfs/devtmpfs/procfs*：`/dev` 与 `/proc` 目前是兼容实现。后续可将它们提升为独立内存文件系统，减少对 ext4 承载虚拟文件内容的依赖。
3. *loop 设备数据路径*：loop ioctl 状态管理已实现，但块读写尚未转发到 backing file。
4. *inotify 事件生产*：watch 管理和 fd 读写框架已经具备，仍需在 create/unlink/rename/write/chmod 等 VFS 操作中插入事件生成钩子；fanotify 的权限事件响应与完整传播语义仍未接入。
5. *异步 IO*：`io_uring_setup` 提供占位 fd 与 ring offsets ABI，但提交/完成环引擎尚未实现；完整 `io_uring`/AIO 数据路径仍需结合统一缓存和 poll 唤醒机制继续扩展。
6. *锁语义补全*：POSIX record lock 的等待图死锁检测、OFD lock 与文件租约已实现；close 时自动释放锁与更贴近 Linux 的租约生命周期仍可继续完善。
7. *xattr 持久化*：xattr 已有真实读写路径，但其持久化存储与一致性仍需进一步验证。

#text(size: 8.5pt, fill: rgb("536471"))[_实现追溯：_ 本章对应 `os/src/fs/`（`vfs.rs`、`fstruct.rs`、`fs_info.rs`、`dcache.rs`、`page_cache.rs`、`mount.rs`、`stat.rs`、`map_dynamic_link.rs`、`ext4_lw/`、`kernel_fs_ops/`、`files/`）、`os/src/syscall/fs/` 以及 `os/src/drivers/disk.rs` 的当前实现。]
