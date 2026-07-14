= 系统调用与文件系统

== 系统调用边界

`syscall::syscall(syscall_id, args)` 以 Linux syscall number 将最多六个寄存器参数分派到 `syscall/task`、`syscall/fs`、`syscall/net`、`syscall/sync`、`signal` 和 `time` 子模块。处理器相关 trap 代码只负责取得 ABI 参数；具体 handler 负责参数解码、fd 查找、用户内存复制和调用内核对象。

返回值遵循 Linux 约定：成功返回非负值，失败返回负 errno。未知或尚未实现的调用不应伪装成功。系统调用 handler 保持薄：通用文件、网络、内存和调度语义应放在对应核心模块，便于不同入口复用。

系统调用的用户可见行为以 Linux 手册页为对照基准 #cite(<linux-man-pages>)；当内核实现尚未覆盖某项语义时，应以明确 errno 或受限行为反映实际能力。

== VFS 与 fd 表

VFS 使用统一的 `File` / inode / superblock 抽象承载对象访问。进程的 `FdTable` 将非负整数 fd 映射到 `Arc<dyn File>` 类对象，`FSInfo` 保存 cwd、root 与 umask 等文件系统上下文。因此普通文件、目录、管道、socket、`eventfd`、`signalfd`、epoll 与部分 proc/dev 文件都可通过 `read`、`write`、`poll`、`ioctl` 等共同入口操作。

#align(center)[
  `openat` / `socket` / `pipe2` → `FdTable` → `File` → { VFS inode | pipe | socket | event object }
]

路径解析、挂载信息和权限检查集中于 fs 及 syscall/fs。`openat` 需要正确处理 dirfd、相对路径、符号链接、创建标志与用户路径拷贝；读写路径必须传播底层错误，不能把失败吞成零长度成功。

== 已接入的文件系统能力

- `ext4_lw/` 通过 lwext4 适配块设备，提供普通文件、目录和元数据的主要后端；页缓存和 dcache 在 fs 中管理。
- `kernel_fs_ops/` 负责内核侧文件系统操作及 proc 相关动态内容；`/proc/<pid>/maps` 与 `pagemap` 等文件按访问时的进程 VMA/页表状态生成，避免把高地址逻辑范围扩为真实 ext4 大文件。
- `files/pipe/` 实现管道、FIFO、等待和 splice 相关路径；其读端、写端关闭和 `SIGPIPE` 语义由统一 File 接口协调。
- `files/epoll/`、inotify、fanotify、mqueue、文件锁、lease 与 xattr 为兼容接口提供对象或状态管理。

== 动态链接与特殊设备

初始文件系统和 ELF 装载路径需要提供动态链接器及其依赖。`devfs`、loop 设备、标准输入输出和 proc 文件为用户态工具提供必要的设备和伪文件接口。挂载、只读检查和跨挂载硬链接等语义必须在路径操作中显式判断，不能只依赖底层 inode 的局部行为。
