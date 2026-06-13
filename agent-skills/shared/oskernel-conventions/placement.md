# 代码放置速查表

与 [SKILL.md](SKILL.md) 配合使用。**新增代码前先读本文件，再写代码**；不要用扫仓库「发现模块化目录」代替本表。

**文件/mod 树怎么组织**（拆分、pub use、可见性）→ [rust-project-layout/SKILL.md](../rust-project-layout/SKILL.md)

## 参考实现（照此拆分）

**epoll**（io 多路复用）已按 syscall 薄层 + fs 语义拆分，扩展 poll/select 或新 File 类型时对照：

```
os/src/syscall/io_mpx/epoll.rs   # sys_*、用户缓冲区、suspend 循环
os/src/fs/files/epoll/
  file.rs      # EpollFile + File trait
  ctl.rs       # ADD/MOD/DEL
  wait.rs      # collect_ready（ET/水平、ONESHOT）
  registry.rs  # epfd → Weak<EpollFile>
  events.rs    # epoll 掩码 ↔ PollEvents
```

## 仓库边界

| 路径 | 放什么 | 禁止放什么 |
|------|--------|------------|
| `os/src/` | 内核全部逻辑 | 用户态程序、测例脚本 |
| `user/src/` | initproc、用户 syscall 封装 | 内核 `sys_*`、页表 |
| `lwext4_rust/` | ext4 绑定 | 业务 VFS 逻辑（应在 `os/src/fs/ext4_lw` 封装） |
| `Docs/` | 文档 | 可执行代码 |

## `os/src/` 顶层模块（`main.rs`）

仅通过 `main.rs` 声明的 crate 根模块；**不要**随意新增顶层 `mod foo`（需有充分理由并在 `main.rs` 注册）。

| 模块 | 职责 |
|------|------|
| `arch` | 架构相关：trap、页表、上下文切换、内存布局、时钟 |
| `syscall` | 系统调用分发与 `sys_*` 入口 |
| `task` | 调度、TCB、进程、内核栈、futex 实现体 |
| `mm` | 地址空间、帧分配、`copy_from_user`、缺页 |
| `fs` | VFS、inode、OSFile、ext4 封装 |
| `net` | smoltcp 协议栈、socket 语义（feature `net`） |
| `drivers` | VirtIO 等硬件驱动 |
| `trap` | trap 入口、异常分发到 syscall/缺页 |
| `signal` | 信号投递与处理框架 |
| `sync` | 内核同步原语（非 futex syscall 包装） |
| `timer` | 时钟与定时器 |
| `utils` | 跨模块小工具（`SysErrNo`、PollSet 等） |
| `config` / `logger` / `console` | 配置与输出 |

## 决策树：我要加的功能放哪？

```
是用户态测试/工具？
  └─ 是 → user/src/bin/ 或 user/src/syscall/

是 Linux 系统调用入口 sys_*？
  └─ 是 → os/src/syscall/<子模块>/，并在 syscall/mod.rs 注册

  文件类（open/read/write/…）     → syscall/fs/
  socket/bind/connect/…         → syscall/net/（薄包装）
  fork/clone/exec/wait/…        → syscall/task/
  mmap/brk                      → syscall/mm/
  futex                         → syscall/sync/
  poll/epoll/select             → syscall/io_mpx/
  信号 syscall                  → syscall/signal.rs 或 task/
  仅 syscall 用到的标志位       → syscall/options.rs

是协议/套接字语义（TCP 状态、组播表）？
  └─ 是 → os/src/net/（tcp.rs、udp.rs、options.rs…）

是网卡/块设备硬件？
  └─ 是 → os/src/drivers/virtio/

是以太网 ARP/组播 MAC 封装？
  └─ 是 → os/src/net/device/

是 VFS/inode/路径解析？
  └─ 是 → os/src/fs/（vfs、ext4_lw、kernel_fs_ops）

是调度/进程树/线程生命周期？
  └─ 是 → os/src/task/

是页表/COW/缺页（与 ISA 相关）？
  └─ 是 → os/src/arch/<arch>/qemu/page_table.rs 等

是页表/地址空间（架构无关接口）？
  └─ 是 → os/src/mm/

仅 RISC-V 或仅 LoongArch？
  └─ 是 → arch/ 下对应子树 + #[cfg(target_arch = "...")]
```

## `syscall/` 子目录

| 子目录/文件 | 内容 |
|-------------|------|
| `mod.rs` | `Syscall` 枚举、`syscall()` 分发 |
| `fs/` | `sys_openat`、`sys_read`、`sys_close`… |
| `net/` | `sys_socket`、`sys_bind`、`sys_setsockopt`… → 调用 `net::` |
| `task/` | `sys_clone`、`sys_execve`、`sys_waitpid`… |
| `mm/` | `sys_mmap`、`sys_munmap`… |
| `sync/` | `sys_futex` |
| `io_mpx/` | `sys_poll`、`sys_epoll_*` |
| `options.rs` | **syscall 层**标志（`WaitOption`、`MmapProt`），不是 socket 选项 |
| `time.rs` / `memory.rs` / `signal.rs` | 按主题单文件 |

**规则**：`syscall/net/*.rs` 只做参数解析、fd 表、调用 `crate::net` / `crate::fs::Socket`；**不要**在 syscall 里写 smoltcp 状态机。

## `net/` 子目录

| 文件/目录 | 内容 |
|-----------|------|
| `socket.rs` | `Socket` 枚举分发 |
| `tcp.rs` / `udp.rs` / `unix.rs` | 协议实现 |
| `options.rs` | `Configurable`、`SetSocketOption`（socket 选项语义） |
| `listen_table.rs` | 监听端口与 accept 队列 |
| `device/ethernet.rs` | ARP、以太网帧、组播 MAC |
| `router.rs` | 路由与 dispatch |
| `mod.rs` | `init_network`、`poll_interfaces` |

**不要**把 `VirtIoNetDev` 放进 `net/`（应在 `drivers/virtio/net.rs`）。

## `fs/` 子目录

| 子目录 | 内容 |
|--------|------|
| `vfs.rs` | VFS、路径、inode trait |
| `ext4_lw/` | lwext4 的 inode/dirent |
| `kernel_fs_ops/` | `open`、`FsIndex`、init 文件 |
| `files/` | `OSFile` 及 `File` trait 实现（pipe、stdio、socket 文件对象） |
| `files/epoll/` | epoll 实例、兴趣列表、就绪收集；`syscall/io_mpx/epoll.rs` 仅 `sys_*` |
| `fstruct.rs` | `FdTable`、`FileDescriptor` |
| `map_dynamic_link.rs` | 动态链接器路径映射 |

## `task/` 子目录

| 子目录 | 内容 |
|--------|------|
| `task/` | `TaskControlBlock`、clone、调度切换 |
| `process/` | `Process`、`exit_and_reparent`、进程元数据 |
| `manager.rs` / `processor.rs` | 就绪队列、运行 CPU |
| `futex.rs` | futex 等待队列（**不是** `sys_futex`） |
| `future/` | `block_on`（网络阻塞用） |

## `arch/` 规则

- **仅** ISA / 板级差异：`trap`、`PageTable`、上下文、`memory_layout`
- 通过 `arch/mod.rs` 的 `cfg_if` 导出 `riscv64` 或 `loongarch64`
- QEMU 与「上板」再分子目录（如 `arch/riscv64/qemu/`）

**禁止**：在 `mm/` 或 `syscall/` 里写汇编或大段 `#[cfg(target_arch)]` 页表逻辑；应封装到 `arch` 再由 `mm` 调用。

## 常见误放（纠正）

| 错误位置 | 应放到 |
|----------|--------|
| `sys_read` 实现在 `fs/vfs.rs` | `syscall/fs/io.rs` |
| smoltcp 轮询写在 `syscall/mod.rs` | `net/mod.rs` 的 `poll_interfaces` |
| VirtIO 队列在 `net/tcp.rs` | `drivers/virtio/net.rs` |
| `JoinGroup` 只在 `syscall/net/opt.rs` | 语义在 `net/tcp.rs` + `options.rs`，syscall 只解析参数 |
| 进程托孤在 `syscall/task/wait.rs` | `task/process/process.rs` |
| futex 队列在 `syscall/sync/futex.rs` 全部实现 | 核心队列 `task/futex.rs`，syscall 层薄包装 |
| 通用 `PollSet` 放在 `net/` | `utils/poll.rs` |
| epoll 边缘触发/收集逻辑在 `syscall/io_mpx/epoll.rs` | `fs/files/epoll/wait.rs` + 薄 `sys_epoll_*` |
| `EPOLL_TABLE` 与 `ctl` 在 syscall | `fs/files/epoll/registry.rs`、`ctl.rs` |
| 测例列表在 `os/src` | `user/src/bin/ltp/filelist.rs` |

## 新文件 vs 扩展现有文件

| 情况 | 建议 |
|------|------|
| 同一主题已有 `sys_*` 文件（如 `stat.rs`） | 追加到该文件 |
| 新一类 syscall（如新的 fd 操作） | `syscall/fs/` 新文件 + `fs/mod.rs` 声明 |
| 单文件已超过 ~400 行且职责可拆分 | 按子职责拆文件，**不要**拆成无意义的 `helpers.rs` |
| 仅一处使用的私有辅助函数 | 留在同一文件底部，或 `mod` 内 `fn` |

## `mod.rs` 职责

1. 声明子模块 `mod xxx;`
2. 精选 `pub use` 对外 API（避免 `pub use self::*` 泛滥）
3. 简短 `//!` 模块文档
4. **避免**在 `mod.rs` 写大段业务逻辑（初始化除外，如 `net/mod.rs` 的 `init_network`）

## 可见性

| 关键字 | 用途 |
|--------|------|
| `pub` | 跨 crate 根模块给其他顶层模块用 |
| `pub(crate)` | 仅内核 crate 内（如 `net::state`） |
| 无 `pub` | 仅本模块及子模块 |

新增全局状态：优先现有 `Lazy`/`Once` 模式（见 `net/mod.rs`、`LISTEN_TABLE`），不要新建随意 `static mut`。

## 条件编译

```rust
#[cfg(feature = "net")]      // Cargo feature
#[cfg(target_arch = "riscv64")]  // 架构
```

- 网络代码：`#[cfg(feature = "net")]` 包裹 `mod net` 与相关 syscall
- 双架构：逻辑放在 `arch/`，不要在 `syscall` 里堆叠过长 `cfg` 块
