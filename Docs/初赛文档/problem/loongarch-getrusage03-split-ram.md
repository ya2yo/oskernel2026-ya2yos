# LoongArch getrusage03 与 QEMU virt 分段内存

### 背景

RISC-V 的 `getrusage03` 后续阶段需要真实触碰 500MiB 匿名内存。此前 RISC-V 修复将 QEMU `-m` 和内核 `PHYSICAL_MEMORY_SIZE` 同步提升到 1GiB，使连续物理内存假设仍然成立。

LoongArch 也遇到 `getrusage03` 卡死，但 LoongArch QEMU `virt` 的内存布局不同，不能直接复用 RISC-V 的连续 1GiB 写法。

### 现象

LoongArch 单跑 `getrusage03` 的 `log.ans` 中，测例已经推进到：

```text
getrusage03.c:43: TPASS: initial.self ~= child.self
getrusage03.c:57: TPASS: initial.children ~= 100MB
getrusage03.c:66: TPASS: child.children == 0
```

随后不再继续输出。启动日志同时显示：

```text
MEMORY_END:     0x9000000020000000
init_cma:
from: 0x9000000003633000
size: 0x1c9cd000
to:   0x9000000020000000
```

这说明旧实现把 LoongArch 物理地址 `0x0..0x20000000` 当作连续 RAM 加入了页帧分配器。

### 分析

RISC-V QEMU `virt` 的 RAM 从 `0x80000000` 开始连续增长。因此 RISC-V 修复只需要：

- `make_scripts/riscv64.mk`：`MEMORY_SIZE := 1G`
- `os/src/arch/riscv64/qemu/memory_layout.rs`：`PHYSICAL_MEMORY_SIZE = 0x4000_0000`

LoongArch QEMU `virt` 不同。实测设备树内存节点显示：

| QEMU 参数 | 低端 RAM | 高端 RAM |
|-----------|----------|----------|
| `-m 512M` | `0x00000000..0x10000000` | `0x80000000..0x90000000` |
| `-m 1G` | `0x00000000..0x10000000` | `0x80000000..0xb0000000` |

中间 `0x10000000..0x80000000` 不是 RAM。旧代码在 512MiB 配置下仍按连续 `0..512M` 初始化 CMA，因此会把 `0x10000000..0x20000000` 这段非 RAM 区域放进页帧分配器。`getrusage03` 有较大内存压力，后续分配到这段洞时就可能表现为卡死、异常或 panic。

### 根因

LoongArch 的物理内存描述错误：把 QEMU `virt` 的分段 RAM 当成了从 `0x0` 开始的连续 RAM。

RISC-V 没有这个问题，是因为 RISC-V 的 1GiB RAM 在当前 QEMU 参数下仍是连续区间；LoongArch 增大 `-m` 后必须显式处理低端 256MiB 和高端 768MiB 两段。

### 修复

#### 1. LoongArch QEMU 内存提升到 1GiB

`make_scripts/loongarch64.mk`：

```make
MEMORY_SIZE := 1G
```

#### 2. LoongArch 描述真实 RAM 分段

`os/src/arch/loongarch64/qemu/memory_layout.rs`：

```rust
pub const PHYSICAL_MEMORY_SIZE: usize = 0x4000_0000;
pub const PHYSICAL_MEMORY_RANGES: &[(usize, usize)] =
    &[(0x0000_0000, 0x1000_0000), (0x8000_0000, 0x3000_0000)];
```

`MEMORY_END` 保留为低端连续 RAM 的结束地址，避免它被误解为完整物理内存的线性上界。完整 RAM 只通过 `PHYSICAL_MEMORY_RANGES` 表达。

#### 3. CMA 初始化按架构分支

`os/src/mm/frame_alloc/buddy_cma.rs`：

- LoongArch：遍历 `PHYSICAL_MEMORY_RANGES`，分别 `add_to_heap(left, range_end)`
- 非 LoongArch：保留原来的连续区间 `init(left, size)` 逻辑

这样 RISC-V 的正常路径不被改动，同时 LoongArch 不再把 MMIO hole 或非 RAM 地址交给页帧分配器。

### 涉及文件

| 文件 | 修改内容 |
|------|----------|
| `make_scripts/loongarch64.mk` | QEMU 内存从 512MiB 提升到 1GiB |
| `os/src/arch/loongarch64/qemu/memory_layout.rs` | 新增 `PHYSICAL_MEMORY_RANGES` 描述两段 RAM |
| `os/src/mm/frame_alloc/buddy_cma.rs` | LoongArch 使用分段 RAM 初始化 CMA，非 LoongArch 保持连续逻辑 |

### 验证

已完成：

```text
cargo fmt --manifest-path os/Cargo.toml
git diff --check
make riscv64-build
```

`make riscv64-build` 通过，说明 RISC-V 连续内存路径未被破坏。

本地执行 `make loongarch64-build` 时，Rust 代码编译走到最终阶段，但宿主环境缺少被 `.incbin` 引入的文件：

```text
Could not find incbin file '/opt/gcc-13.2.0-loongarch64-linux-gnu/loongarch64-linux-gnu/lib64/libgcc_s.so.1'
```

该失败与本次内存布局修复无关。需要在具备完整 LoongArch 工具链文件的环境中继续运行 `getrusage03` 验证最终通过情况。
