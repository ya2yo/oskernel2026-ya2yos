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

## 独立复现入口（2026-07-21）

官方 `buildstorm_testcode.sh` 位于只读测例仓，最终镜像不会在构建时自动安装该脚本，
因此不能只在外部仓拆分后期待 guest 使用新文件。为保持与正式测例一致的
`busybox sh <script>` 执行方式，`create_init_files()` 现在仅在同时发现 `/glibc` 与
`/root/.cargo` 的 BuildStorm 根文件系统时，通过 `write_executable_init_file()` 写入五个
诊断脚本：

- `buildstorm_toolchain_debug.sh`
- `buildstorm_minibuild_prepare_debug.sh`
- `buildstorm_minibuild_build_debug.sh`
- `buildstorm_xtask_prebuild_debug.sh`
- `buildstorm_xtask_build_debug.sh`

它们分别覆盖工具链、MINIBUILD 的 `rm/cargo new` 与 `cargo build`、`tg-xtask`
预构建和正式 `xtask` 编译。每项重新设置原脚本的挂载与 Rust 环境；MINIBUILD build
仍保留 `cargo build >/dev/null 2>&1`，避免改变待诊断的 fd/pipe 拓扑。

所有新脚本只输出 `BUILDSTORM_DEBUG_*`，不输出评分器会匹配的正式
`BUILDSTORM_TOOLCHAIN`、`BUILDSTORM_MINIBUILD` 或 `BUILDSTORM_COMPILE` 标记。这样，
局部调试不会把部分执行误报为竞赛得分。`initproc` 当前只顺序运行 MINIBUILD 的
prepare 和 build 两项；prepare 失败时不会进入 build。

## 验证

- `make riscv64-build`：通过。
- `git diff --check`：通过。
- `timeout 600s make run TARGET_ARCH=riscv64 > log.ans 2>&1`：未通过完整 BuildStorm；日志停在 `BUILDSTORM_TOOLCHAIN ok`，因此本问题仍待继续定位。
- `make build-arch TARGET_ARCH=riscv64`：独立脚本和入口编译通过。
- `make build-arch TARGET_ARCH=loongarch64`：同一启动期脚本路径编译通过；本轮未运行
  LoongArch64 QEMU 行为回归。
- `timeout 180s make run TARGET_ARCH=riscv64 > /tmp/buildstorm-minibuild-split-riscv.log 2>&1`：
  串口依次输出 `BUILDSTORM_DEBUG_MINIBUILD_PREPARE begin`、`ok` 和
  `BUILDSTORM_DEBUG_MINIBUILD_BUILD begin`，之后没有 `BUILDSTORM_DEBUG_MINIBUILD ok/fail`、
  panic、TFAIL 或 TBROK。该宿主采集在正常 guest `shutdown!` 前中断，故它证明了卡点
  已被隔离到原始 `cargo build`，但不把这一次短样本表述为完整的 180 秒死锁证明。
- 命令行强制 `MEMORY_SIZE=2G SMP=2` 的尝试未进入用户态：当前内核的 CMA 布局按 8GiB
  初始化，在 `init_cma_late()` 前后停止；该样本不用于判断 MINIBUILD 语义。

## 后续

应在不改变 `buildstorm_testcode.sh` 语义的前提下，采集 `cargo build` 中 Rust 进程的精确 syscall、页故障和调度状态，并以 `BUILDSTORM_MINIBUILD ok` 为下一阶段验收门槛。
