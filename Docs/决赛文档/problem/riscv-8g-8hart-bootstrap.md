# RISC-V 8GiB CMA 阶数越界与八核启动栈破坏

## 背景

评测机配置调整为 RISC-V QEMU `-m 8G -smp 8` 后，维护者提供的 `log.ans` 在内核启动阶段
panic，尚未进入用户态测例。

## 现象

原始日志在激活完整内核页表后执行 `init_cma_late()`，输出：

```text
init_cma_late:
from: 0xffffffc0c0000000
size: 0x1c0000000
to:   0xffffffc280000000
panic
[kernel] Panicked at .../buddy_system_allocator/src/lib.rs:90
index out of bounds: the len is 32 but the index is 32
```

修复该 panic 后，第一次 8 核运行又停在 `mm:cma inited`，没有打印 `kernel token`。单核对照可以
通过 CMA 和页表初始化，但在尝试启动不存在的 hart 时按预期失败，说明问题只在八核启动配置。

## 分析

RISC-V CMA 在 bootstrap 页表激活前仅纳入首个 1GiB；完整页表激活后将剩余 7GiB 加入同一个
buddy allocator。`add_to_heap()` 按对齐的最大二次幂拆分该区间，包含 4GiB 块；其阶数是 32，
而 upstream `buddy_system_allocator 0.6.0` 的 `free_list` 固定为 32 项，只能索引 `0..31`。

同时，Rust 的 `HART_NUM` 已更新为 8，但 `entry.asm` 的 `BOOT_HARTS` 保持为 2。QEMU 允许任意
hart 成为 bootstrap hart；日志中分别出现 HART5 和 HART7。汇编按 hart id 计算
`boot_stack_top - hart_id * BOOT_STACK_SIZE`，因此 hart id 大于 1 会落到两块预留启动栈外并覆盖
`.bss`，使 CMA 后的首个页表页分配卡死。

## 修复

- 将修改后的 `buddy_system_allocator` 从 `os/vendor/` 迁移到
  `crates/buddy_system_allocator`，并在 `os/Cargo.toml` 以 path dependency 显式引用，避免
  项目特定修复混入通用 vendored source。
- allocator 的 free-list 数量扩展为 34，覆盖 order 0 到 33，可表示完整 8GiB 块；byte/frame
  两种 allocator 共用该常量。`FrameAllocator` 改用 `core::array::from_fn`，兼容旧工具链对
  长度大于 32 的数组缺少 `Default` 实现。
- 伙伴释放合并只在存在更高阶链表时继续，避免最高可表示阶继续合并时访问数组末尾之外。
- 将 RISC-V `BOOT_HARTS` 从 2 同步为 8，和 `arch::config::HART_NUM`、QEMU `-smp 8` 保持一致。

## 涉及文件

| 文件 | 修改 |
| --- | --- |
| `crates/buddy_system_allocator/` | 本地维护的 allocator，支持 8GiB CMA 和最高阶安全合并 |
| `os/Cargo.toml`、`os/Cargo.lock` | 切换到根目录本地 path dependency |
| `os/src/arch/riscv64/qemu/asms/entry.asm` | 预留 8 个 128KiB bootstrap stack |

## 验证

- `make riscv64-build`：通过。
- `make loongarch64-build`：通过，确认本地 allocator crate 可被两架构解析。
- 默认 `8G / 8` RISC-V QEMU 运行 60 秒并写入根目录 `log.ans`：通过原 panic 点，出现
  `mm:cma late range inited`、`remap_test passed!`、`boot secondary harts...complete.`，7 个 AP
  均打印启动消息，随后进入 `BUILDSTORM_TOOLCHAIN ok`；未出现 `panic`、`TFAIL` 或 `TBROK`。

该运行由 60 秒外部 timeout 截止，未将完整 BuildStorm 编译或全量测试标记为通过。
