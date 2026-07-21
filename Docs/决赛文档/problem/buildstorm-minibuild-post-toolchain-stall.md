# BuildStorm MINIBUILD mmap 预算与动态栈 fork EFAULT

## 背景

final-2026 的 `buildstorm_testcode.sh` 在验证 `rustc --version` 和 `cargo --version` 后，会创建 `/tmp/minibuild` 并执行一次 `cargo build`。该阶段是后续 `cargo xtask` 正式编译前的最小 Rust 编译闭环。

## 现象

根目录 `log.ans` 稳定输出 `BUILDSTORM_TOOLCHAIN ok`，随后不出现 `BUILDSTORM_MINIBUILD ok` 或 `BUILDSTORM_MINIBUILD fail`。在 RISC-V `8G / 8 CPU` QEMU 上，最近一次定向运行到 10 分钟外部上限仍停在该阶段；没有观察到 `panic`、`TFAIL` 或 `TBROK`。

临时 shell 跟踪确认 `rm -rf /tmp/minibuild` 和 `cargo new` 已完成，卡点位于脚本重定向输出的 `cargo build`。对 guest Rust 子进程的采样显示其仍在进行 `read`、`clone3`、`ppoll` 和 pipe 操作，尚未证明为 futex、pipe 或锁顺序死锁。

## 已确认根因与修复（2026-07-21）

将 MINIBUILD 拆为独立入口后，`/tmp/buildstorm-vfork-after.log` 中的 Rustc
进程（PID 10/TID 13）明确暴露了失败路径：匿名 `mmap` 申请 128 MiB
`PROT_NONE | MAP_PRIVATE | MAP_ANONYMOUS` 时，已有
`total_mmap_size=491208704`，`mmap_ops.rs` 按 512 MiB 累计预算拒绝请求并返回
`ENOMEM`；其 64 MiB fallback 也被拒绝。该路径并不是单次 512 MiB 映射，也不是
物理页耗尽。

`MemorySetInner::mmap()` 对这类映射只插入 lazy VMA，物理帧在后续 page fault
才分配；而用户地址空间有 192 GiB，远大于该 512 MiB 人为预算。Rustc 会同时预留
多个 128 MiB 的 arena，因此现有预算会在实际编译开始后稳定失败。

将 RISC-V 与 LoongArch64 的 `MAX_MMAP_SIZE` 从 512 MiB 提升到 2 GiB。该改动仍
保留单进程 VMA 上限及缺页时的物理页约束，只允许 Rustc 所需的未驻留地址空间预留。

该修复消除了已记录的 mmap `ENOMEM`，但不是 MINIBUILD 的全部问题。强制重新创建
`/tmp/minibuild` 后，Cargo 已进入实际 `Compiling minibuild`，随后报告
`Bad address (os error 14)`。后续审计确认这不是 Rustc 的 `execve` 用户参数复制错误：
Cargo worker 的普通 `clone()` 在创建 Rustc 子进程时返回 `EFAULT`。fork 克隆跳过了动态
`MAP_STACK` VMA，随后写 `CLONE_CHILD_SETTID` 的用户地址找不到 child VMA。该独立 fork
地址空间复制问题仍待修复。

## 已采纳的 lwext4 大文件缓存探测优化（2026-07-21）

`librustc_driver-37ff94a6423d6d34.so` 的映射长度约为 198 MiB，远超 lwext4
whole-file write-back cache 的 4 MiB 上限。原 `Ext4File::check_cached()` 在大小判断前
打印 `initialize cache!`，且 `file_seek()` 会在每次 mmap 页读取时再次调用它；超限后虽然
不建立缓存，却会重复执行 `ext4_fopen()`、`ext4_fsize()`、`ext4_fclose()`。

修复只改动 `Ext4File`，不引入此前有偏移风险的 `file_seek_uncached()`：

- `initialize cache!` 移到 `insert_cache()`/`insert_fifo()` 成功后，因此只表示真实建立了
  小文件缓存。
- `cache_too_large` 是 `Ext4File` 实例级负状态。首次确认文件超过 4 MiB 后，后续
  `file_seek()` 跳过准入探测，仍优先检查已有 `VFileCache`，再走原有底层 descriptor
  seek/read，不会改变小文件缓存偏移语义。
- 成功的跨阈值写入、`file_truncate()`、`O_TRUNC` 打开和删除路径会更新或清除该状态；
  VFS rename 会重建 `Ext4File`，自然重新探测新路径。

该状态按 `Ext4File` 保存而非全局 path 表，避免 rename、hard link、unlink 和路径复用时
引入额外失效表或锁顺序。独立 inode 实例仍各自做一次尺寸探测，这是可接受的性能边界。
本轮仍未采纳只读 `MAP_PRIVATE` 页复用或预读实验，避免把 `mprotect(PROT_WRITE)` 的 COW
隔离问题混入本修复。

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
局部调试不会把部分执行误报为竞赛得分。`initproc` 当前选择 MINIBUILD build 单项；
prepare 路径保留为独立诊断入口，必要时可先运行以强制创建干净项目。

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make log TARGET_ARCH=riscv64`：通过。
- `timeout 180s make run TARGET_ARCH=riscv64 > /tmp/buildstorm-mmap-2g.log 2>&1`：
  输出 `BUILDSTORM_DEBUG_MINIBUILD_BUILD begin`、`BUILDSTORM_DEBUG_MINIBUILD ok` 和
  `shutdown!`；该次复用已有 `/tmp/minibuild`，仅作为 mmap 上限解除后的快速回归，
  不能证明干净项目已重新编译。
- `timeout 240s make run TARGET_ARCH=riscv64 > /tmp/buildstorm-mmap-2g-fresh-cargo.log 2>&1`：
  强制 prepare 后输出 `BUILDSTORM_DEBUG_MINIBUILD_PREPARE ok` 和
  `BUILDSTORM_DEBUG_MINIBUILD_BUILD begin`，Cargo 进入 `Compiling minibuild`，但其 worker
  创建 Rustc 子进程的普通 `clone()` 返回 `Bad address (os error 14)`，因此完整 MINIBUILD
  尚未通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过；本次没有运行 LoongArch64 QEMU，
  因为当前复现入口和 final-2026 Rust 工具链镜像是 RISC-V 专用。
- `make`：RISC-V 与 LoongArch64 release 构建均通过。
- `make log TARGET_ARCH=riscv64`：通过。
- `timeout 180s make run TARGET_ARCH=riscv64 > /tmp/lwext4-cache-probe-riscv.log 2>&1`：
  输出 `BUILDSTORM_DEBUG_MINIBUILD ok` 和 `shutdown!`，无 panic、`TFAIL` 或 `TBROK`。
  与修复前 `log.ans` 的 16,191 条 `initialize cache!`（其中目标 DSO 为 16,133 条）相比，
  新日志为 68 条，目标 DSO 为 0 条。

### 修复前的诊断记录

- `timeout 600s make run TARGET_ARCH=riscv64 > log.ans 2>&1`：日志停在
  `BUILDSTORM_TOOLCHAIN ok`，尚未进入本次已定位的 mmap 失败信号。
- `timeout 180s make run TARGET_ARCH=riscv64 > /tmp/buildstorm-minibuild-split-riscv.log 2>&1`：
  串口依次输出 `BUILDSTORM_DEBUG_MINIBUILD_PREPARE begin`、`ok` 和
  `BUILDSTORM_DEBUG_MINIBUILD_BUILD begin`，之后没有 `BUILDSTORM_DEBUG_MINIBUILD ok/fail`、
  panic、TFAIL 或 TBROK。该宿主采集在正常 guest `shutdown!` 前中断，故它证明了卡点
  已被隔离到原始 `cargo build`，但不把这一次短样本表述为完整的 180 秒死锁证明。
- 命令行强制 `MEMORY_SIZE=2G SMP=2` 的尝试未进入用户态：当前内核的 CMA 布局按 8GiB
  初始化，在 `init_cma_late()` 前后停止；该样本不用于判断 MINIBUILD 语义。

## 后续

下一步修复普通 fork 对动态 `MAP_STACK` VMA 的继承，再以正式
`buildstorm_testcode.sh` 的 `BUILDSTORM_MINIBUILD` 和 `BUILDSTORM_COMPILE` 标记验证
完整 BuildStorm。另有一个独立的 mmap 记账问题待处理：`munmap()` 目前不会回收
`MAP_STACK` 的预算，且只处理完整覆盖的 VMA；它不是本次 Rustc ENOMEM 的首个阻塞点，
不应混入本修复。
