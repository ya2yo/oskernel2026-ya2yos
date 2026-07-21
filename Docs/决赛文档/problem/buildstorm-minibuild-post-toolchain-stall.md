# BuildStorm 工具链检查后 minibuild 超时分析（待续）

## 背景

final-2026 的 `buildstorm_testcode.sh` 在验证 `rustc --version` 和 `cargo --version` 后，会创建 `/tmp/minibuild` 并执行一次 `cargo build`。该阶段是后续 `cargo xtask` 正式编译前的最小 Rust 编译闭环。

## 现象

根目录 `log.ans` 稳定输出 `BUILDSTORM_TOOLCHAIN ok`，随后不出现 `BUILDSTORM_MINIBUILD ok` 或 `BUILDSTORM_MINIBUILD fail`。在 RISC-V `8G / 8 CPU` QEMU 上，最近一次定向运行到 10 分钟外部上限仍停在该阶段；没有观察到 `panic`、`TFAIL` 或 `TBROK`。

临时 shell 跟踪确认 `rm -rf /tmp/minibuild` 和 `cargo new` 已完成，卡点位于脚本重定向输出的 `cargo build`。对 guest Rust 子进程的采样显示其仍在进行 `read`、`clone3`、`ppoll` 和 pipe 操作，尚未证明为 futex、pipe 或锁顺序死锁。

## 分析

Rust 工具链中的 `librustc_driver-*.so` 约为 300 MiB。原 lwext4 读取路径中，文件映射的每个 4 KiB 缺页都会进入小文件 write-back cache 的准入检查；对超过 4 MiB 上限的文件，这会反复打开并测量同一大文件。另一个限制是原文件页缓存只给 `MAP_SHARED` 映射复用页帧，而动态链接器的只读 `MAP_PRIVATE` DSO 映射会重复读取 clean page。

这些是已确认的低效路径，但尚不能将它们单独认定为 BuildStorm 超时的根因。一次 64 KiB/256 KiB 页缓存预读实验未缩短 minibuild 的可观察完成时间，已撤回，避免引入没有验证收益的内存占用与行为复杂度。

## 当前修改

- `crates/lwext4_rust/src/file.rs`：为超过小文件缓存上限或缓存探测失败的 `Ext4File` 记录 bypass 状态；新增 `file_seek_uncached()`，仅更新 lwext4 descriptor 的位置。
- `os/src/fs/ext4_lw/inode.rs`：`read_at()` 走 uncached seek，避免 mmap/ELF 页读取反复进入 lwext4 write-back cache 准入。
- `os/src/mm/page_fault_handler.rs`：对无写权限的文件后备 `MAP_PRIVATE` 映射，使用现有 `FILE_PAGE_CACHE` 复用 clean page；可写 private 映射仍保持原 COW 路径。

上述三个源码修改暂不提交。复核发现 `file_seek_uncached()` 不会推进已命中
`VFileCache` 的缓存偏移，可能使小文件 `read_at(off)` 从旧偏移读取；同时只读
`MAP_PRIVATE` 若经 `mprotect(PROT_WRITE)` 变为可写，现有实现尚未保证先分裂为
COW 私有页。这两项语义风险必须先修正并回归后才能合入。

## 验证

- `make riscv64-build`：通过。
- `make loongarch64-build`：在本轮代码修改后尚未重新运行；此前同一组源码改动的 LoongArch64 release 构建通过。
- `git diff --check`：通过。
- `timeout 600s make run TARGET_ARCH=riscv64 > log.ans 2>&1`：未通过完整 BuildStorm；日志停在 `BUILDSTORM_TOOLCHAIN ok`，因此本问题仍待继续定位。

## 后续

应在不改变 `buildstorm_testcode.sh` 语义的前提下，采集 `cargo build` 中 Rust 进程的精确 syscall、页故障和调度状态，并以 `BUILDSTORM_MINIBUILD ok` 为下一阶段验收门槛。
