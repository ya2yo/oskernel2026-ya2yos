# Rust 模块组织模式（Ya2yOS）

与 [SKILL.md](SKILL.md)、[placement.md](../oskernel-conventions/placement.md) 配合使用。

---

## 模块形态

Rust 允许的三种等价形态；**本仓库优先与同级目录保持一致**。

| 形态 | 路径 | 何时用 |
|------|------|--------|
| 单文件模块 | `foo.rs` + 父级 `mod foo;` | 单一职责、体量小（如 `syscall/fs/stat.rs`） |
| 目录模块 | `foo/mod.rs` + `foo/bar.rs` | 多子职责（如 `fs/files/epoll/`） |
| 内联子模块 | 同文件 `mod inner { ... }` | 仅父文件使用的 tiny 类型/测试，**少用于内核业务** |

**不要**在同一目录混用 `foo.rs` 与 `foo/mod.rs`（Rust 不允许）。

父级声明示例（与 `fs/files/epoll/mod.rs` 一致）：

```rust
mod ctl;
mod events;
mod file;
mod registry;
mod wait;

pub use file::{EpollCreateFlags, EpollFile};
pub use wait::EpollReady;
```

---

## `mod.rs` 职责

1. `mod child;` 声明子模块
2. 精选 `pub use` — 只 re-export 跨模块需要的 API
3. 顶部 `//!` 说明本目录职责及与 syscall 层的关系
4. 模块级初始化（如 `net/mod.rs` 的 `init_network`）可留在此处
5. **避免**在 `mod.rs` 写大段业务逻辑或 `pub use self::*` 泛滥

---

## `pub use` 与可见性

| 关键字 | 用途 | 示例 |
|--------|------|------|
| `pub` | 供其他**顶层模块**（`syscall`、`task`…）使用 | `pub struct EpollFile` |
| `pub(crate)` | 仅 `os` crate 内，不暴露给未来子 crate | `pub(crate) mod state`（`net/state`） |
| 无修饰 | 仅本模块及子模块 | 内部 `fn collect_ready` |

新增全局状态：沿用 `Lazy` / `Once` / `Mutex`（见 `net/mod.rs`、`LISTEN_TABLE`），**禁止**随意 `static mut`。

跨模块调用方向（与 placement 一致）：

```
syscall/*  →  fs / net / task / mm  （薄包装调用领域 API）
arch/*     ←  mm / trap              （ISA 细节不泄漏到 syscall）
drivers/*  ←  net                    （硬件不进 net/）
```

---

## 文件拆分

| 情况 | 做法 |
|------|------|
| 单文件 > ~400 行且职责可拆 | 按子职责拆成多个 `.rs`，目录 + `mod.rs` |
| 仅一处使用的私有辅助 | 留在同文件底部，不单独建 `helpers.rs` |
| 新一类 syscall | `syscall/<子目录>/新文件.rs` + 父 `mod.rs` 声明 |
| 同主题已有文件 | 追加函数，不平行新建 `xxx2.rs` |

**禁止的文件名**：`helpers.rs`、`misc.rs`、`utils.rs`（子目录内）、`common.rs` — 用职责命名（`ctl.rs`、`wait.rs`、`registry.rs`）。

---

## 条件编译

```rust
#[cfg(feature = "net")]           // Cargo feature（Makefile → os/Cargo.toml）
#[cfg(target_arch = "riscv64")] // 架构差异
```

- 网络：`#[cfg(feature = "net")]` 包裹 `mod net` 与相关 syscall
- 双架构：逻辑放在 `arch/<riscv64|loongarch64>/`，`arch/mod.rs` 用 `cfg_if` 统一导出（见下）
- **避免**在 `syscall/` 堆叠超过 ~10 行的 `#[cfg]` 块 — 下沉到 `arch` 或领域模块

`arch/mod.rs` 范例：

```rust
cfg_if::cfg_if! {
    if #[cfg(target_arch = "riscv64")] {
        mod riscv64;
        pub use riscv64::*;
    } else if #[cfg(target_arch = "loongarch64")] {
        mod loongarch64;
        pub use loongarch64::*;
    }
}
```

---

## 本仓库参考结构

### epoll — syscall 薄 + fs 语义

```
os/src/syscall/io_mpx/epoll.rs    # sys_epoll_*、copy_from_user、suspend
os/src/fs/files/epoll/
  mod.rs       # mod 声明 + pub use
  file.rs      # EpollFile + File trait
  ctl.rs       # ADD/MOD/DEL
  wait.rs      # collect_ready
  registry.rs  # 全局 epfd 表
  events.rs    # 掩码转换
```

### syscall/fs — 按 syscall 主题分文件

```
os/src/syscall/fs/
  mod.rs       # mod 声明 + pub use 聚合
  io.rs        # read/write
  stat.rs      # stat/fstat
  pipe.rs      # pipe 相关 sys_*
  ...
```

### net — 协议与选项分离

```
os/src/net/
  mod.rs           # init_network、poll_interfaces
  tcp.rs / udp.rs  # 协议实现
  options.rs       # socket 选项语义（pub mod options）
  device/          # 以太网/ARP
  state.rs         # pub(crate) 内部状态
```

VirtIO 硬件在 `drivers/virtio/`，不在 `net/`。

### user crate

```
user/src/
  lib.rs           # crate 根，mod 声明
  syscall/         # 用户态 syscall 封装
  bin/             # initproc、测例（每个 bin 一个文件）
```

用户测例不进 `os/src/`。

---

## 反模式

| 反模式 | 正确做法 |
|--------|----------|
| 在 `syscall/` 实现完整状态机 | 语义进 `fs/` / `net/` / `task/`，syscall 薄包装 |
| `mod.rs` 数百行业务代码 | 拆到子 `.rs`，mod.rs 只声明与 re-export |
| `pub use self::*` 导出整个子树 | 按需 `pub use` 具体类型 |
| 新建顶层 `mod foo` 无充分理由 | 用 placement 现有顶层模块 |
| 把跨子系统逻辑塞进 `utils/` | 按职责归位到对应模块 |
| 用扫仓库「发现目录很模块化」定规范 | 以 placement + 本 patterns 为准 |
| `helpers.rs` 收纳无关函数 | 按职责命名文件或留私有 fn |

---

## 重构检查命令

结构变更后至少：

```bash
make          # 或 TARGET_ARCH=loongarch64 make
```

若移动了 `#[no_mangle]`、链接脚本可见符号或 feature 门控模块，两侧架构各编一次。
