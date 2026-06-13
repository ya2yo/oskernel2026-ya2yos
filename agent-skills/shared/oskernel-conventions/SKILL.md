---
name: oskernel-conventions
description: >-
  Ya2yOS 内核 Rust 代码规范：模块放置、目录职责、命名与错误处理，防止代码写错位置。
  用于新增/移动代码、代码审查、或不确定 sys_*、net、fs、arch 应放在哪时。
  完整流程见 kernel-change，目录速查见 placement.md。
---

# OSKernel 代码规范

> 改内核流程：[kernel-change](../kernel-change/SKILL.md)  
> **代码放哪**：[placement.md](placement.md)（必读，防误放）  
> **mod 怎么拆/怎么写**：[rust-project-layout](../rust-project-layout/SKILL.md)  
> 文档：[doc-writing](../doc-writing/SKILL.md)

---

## Agent 必读顺序（勿跳过）

1. **先读** [placement.md](placement.md)（决策树 + 误放表）
2. **再读** 目标目录里已有文件（对齐风格）
3. **禁止** 用「发现某目录很模块化」代替第 1 步（例如 epoll 已在 `fs/files/epoll/`，仍须以 placement 为准扩展）

---

## 参考实现：epoll（syscall 薄 + fs 语义）

| 职责 | 路径 |
|------|------|
| `sys_epoll_*`、用户 `epoll_event`、超时挂起 | `os/src/syscall/io_mpx/epoll.rs` |
| `EpollFile`、`ctl_*`、`collect_ready`、`EPOLL_TABLE` | `os/src/fs/files/epoll/` |

新增 poll/select 或类似机制时 **按同一模式拆分**，见 placement 中 `files/epoll/` 与误放表。

---

## 核心原则（Rust 项目惯例）

1. **按职责分 crate 模块**，不按「方便」堆进 `utils` 或 `mod.rs`
2. **syscall 层只做入口**，业务在 `fs` / `net` / `task` / `mm`
3. **架构相关只进 `arch/`**，硬件只进 `drivers/`
4. **用户态与内核严格分离**：`user/` vs `os/src/`
5. 新增前 **先读同目录现有文件**，保持相同 `mod` + `pub use` 风格

---

## 代码放置（最重要）

### 快速对照

| 你要写… | 目录 |
|---------|------|
| `sys_*` 函数 | `os/src/syscall/<fs\|net\|task\|mm\|sync\|io_mpx>/` |
| TCP/UDP/socket 选项语义 | `os/src/net/` |
| `sys_setsockopt` 参数解析 | `os/src/syscall/net/opt.rs` |
| VirtIO 网卡/磁盘 | `os/src/drivers/virtio/` |
| 页表/COW/trap | `os/src/arch/<riscv64\|loongarch64>/` |
| 地址空间、用户拷贝 | `os/src/mm/`（`copy_from_user`） |
| VFS、ext4、fd 表 | `os/src/fs/` |
| 调度、进程、futex 队列 | `os/src/task/` |
| initproc、测例 | `user/src/bin/` |

### 禁止误放

- 不要在 `syscall/` 实现 smoltcp 状态机 → 用 `net/`
- 不要在 `net/` 写 VirtIO 寄存器操作 → 用 `drivers/virtio/`
- 不要在 `fs/vfs.rs` 写 `sys_open` → 用 `syscall/fs/`
- 不要把 socket 选项枚举塞进 `syscall/options.rs` → socket 用 `net/options.rs`
- 不要在 `utils/` 塞整块子系统逻辑

完整决策树、子目录表、误放纠正表 → **[placement.md](placement.md)**

---

## 模块与文件命名

| 类型 | 约定 | 示例 |
|------|------|------|
| 源文件 | `snake_case.rs` | `listen_table.rs` |
| 模块目录 | `snake_case/` | `kernel_fs_ops/` |
| 类型/trait | `PascalCase` | `TcpSocket` |
| 函数 | `snake_case` | `poll_interfaces` |
| 系统调用 | `sys_` 前缀 | `sys_read` |
| 常量 | `SCREAMING_SNAKE` | `TCP_RX_BUF_LEN` |
| 私有字段 | 前缀 `_`（可选） | `_handle` |

## 新增系统调用时的位置

1. `syscall/mod.rs`：`Syscall` 枚举 + `syscall()` 分支（Linux 号一致）
2. 实现文件：按主题放入 `syscall/fs/`、`syscall/net/` 等
3. 若需内核能力：在 `fs`/`net`/`task` 增加或扩展类型方法，**不在 syscall 里堆逻辑**

详见 [syscall-implementation](../syscall-implementation/SKILL.md)。

---

## 错误处理与锁

```rust
// 系统调用
pub fn sys_xxx(...) -> SyscallRet {
    let task = current_task()?;
    let proc = task.process.inner_lock();
    // ...
}

// 内部模块
pub fn do_xxx(...) -> SysResult<T> { ... }
```

- 用户指针：**必须** `copy_from_user` / `copy_to_user`，禁止 `translated_str().unwrap()`
- syscall 包装：`match { ... }?` 后 `Ok(0)`，禁止吞掉 `Err`
- **锁顺序**：先 `task`，后 `process`（`fd_table` 在 process 内）

---

## 注释

- 每个模块文件顶部：`//!` 说明职责
- 公共 API：`///` + 参数/返回值（不必冗长）
- 非显而易见的协议/竞态：简短行内说明

---

## 条件编译

```toml
# os/Cargo.toml — 编译时由 Makefile 指定，勿与 default features 冲突
riscv64 / loongarch64 / net
```

```rust
#[cfg(feature = "net")]
#[cfg(target_arch = "riscv64")]
```

双架构差异 → [dual-arch](../dual-arch/SKILL.md)。

---

## 日志

```bash
make log    # debug!
make        # warn+
```

`debug!` / `warn!` / `error!`；排查测例用 `log.ans`（见 [build-and-test](../build-and-test/SKILL.md)）。

---

## 文档与 AI

非平凡改动后按 [doc-writing](../doc-writing/SKILL.md) 更新开发日志、`problem/` 等。

---

## 相关技能

| 技能 | 用途 |
|------|------|
| [placement.md](placement.md) | 目录与误放速查 |
| [rust-project-layout](../rust-project-layout/SKILL.md) | Rust mod 拆分与 pub use |
| [kernel-change](../kernel-change/SKILL.md) | 改代码全流程 |
| [syscall-implementation](../syscall-implementation/SKILL.md) | syscall 三步 |
| [dual-arch](../dual-arch/SKILL.md) | 双架构 |
| [doc-writing](../doc-writing/SKILL.md) | 文档 |
