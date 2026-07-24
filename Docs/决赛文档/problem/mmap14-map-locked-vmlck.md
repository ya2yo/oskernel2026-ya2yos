# LTP mmap14 MAP_LOCKED 与 VmLck 统计修复

## 背景

`mmap14` 验证匿名私有 `mmap()` 使用 `MAP_LOCKED` 后，进程的 `/proc/self/status` 是否立即反映锁定内存大小。

## 现象

根目录 `log.ans` 中 musl 和 glibc 两轮都报告 `Expected 1024K locked, get 0K locked`，没有 panic 或其他测试错误。

## 分析

测试通过 `mmap(..., MAP_PRIVATE | MAP_LOCKED | MAP_ANONYMOUS, ...)` 建立 1 MiB 映射，随后读取 `/proc/self/status` 的 `VmLck`。内核 `MmapFlags` 未包含 `MAP_LOCKED`，`from_bits_truncate` 将该位丢弃；同时动态生成的 status 只有 `VmRSS` 和 `VmSwap`，没有 `VmLck`，所以测试只能读到初始化文件中的零值。

## 根因

`MAP_LOCKED` 没有进入 VMA 元数据，且 proc status 缺少 Linux 兼容的 `VmLck` 字段，导致锁定内存没有可观察的内核统计。

## 修复

- 在 `MmapFlags` 中接入架构对应的 `MAP_LOCKED` 常量，让 mmap VMA 保留该标志。
- `MemorySetInner::locked_size_kb()` 按当前 VMA 范围统计带 `MAP_LOCKED` 的页数；由于 `munmap()` 和 VMA 拆分直接修改 `areas`，统计会随映射生命周期更新。
- `/proc/<pid>/status` 输出 `VmLck`。统计基于 VMA 而非已分配物理帧，因此惰性 mmap 建立后即可反映 1 MiB 锁定范围；本内核无 swap，现有 mlock 系列仍保持参数校验和驻留内存模型。

## 涉及文件

- `os/src/syscall/options.rs`
- `os/src/mm/memory_set/accessors.rs`
- `os/src/mm/memory_set/handle.rs`
- `os/src/fs/kernel_fs_ops/proc_file.rs`

## 验证

- `make`：RISC-V、LoongArch64 release 构建均通过，仅有既有 vendored `smoltcp` warning。
- `timeout 90s make run > log.ans 2>&1`：musl/glibc 的 `mmap14` 均 `TPASS`，summary 均为 `passed 1 failed 0 broken 0`，最终输出 `shutdown!`；无 `TFAIL`、`TBROK`、panic 或错误日志。
- `git diff --check`：通过。
