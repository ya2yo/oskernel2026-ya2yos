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
`MAP_STACK` VMA，随后写 `CLONE_CHILD_SETTID` 的用户地址找不到 child VMA。

该独立 fork 地址空间复制问题已修复。`from_existed_user()` 现在将
`MapAreaType::Stack` 且带 `MAP_STACK` 的动态 VMA 作为 mmap 区域参与 fork 的
COW / shared-memory 克隆；只有固定初始 stack 和 trap VMA 保持由 child 重建。普通 fork
查找 child 的固定 stack 时显式排除 `MAP_STACK`，避免把已继承的动态栈误认为初始 stack。
这样 `CLONE_CHILD_SETTID` 写入时，子地址空间已包含对应的动态栈地址。

本轮未重构 `clone_process()` 其他后置可失败步骤的通用回滚路径。修复前的 EFAULT 会留下
未运行的 child process 记录；本次只消除已定位的触发条件，不将该独立清理问题混入动态栈
地址空间修复。

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

## 独立复现入口（2026-07-21，历史实现）

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
- `make build-arch TARGET_ARCH=riscv64`、`make build-arch TARGET_ARCH=loongarch64`：通过。
  只有既有 Cargo config 弃用和 vendored smoltcp warnings。
- `make log TARGET_ARCH=riscv64`：通过。
- `timeout 300s make run TARGET_ARCH=riscv64 > /tmp/buildstorm-map-stack-fork-riscv-final.log 2>&1`：
  fresh `prepare -> build` 输出 `BUILDSTORM_DEBUG_MINIBUILD_PREPARE ok` 和
  `BUILDSTORM_DEBUG_MINIBUILD_BUILD begin`。原失败的 Cargo worker
  `clone(CLONE_CHILD_CLEARTID | CLONE_CHILD_SETTID)` 现返回 PID 32，子进程已实际运行；
  采集范围内没有 `Clone ret = Bad address` 或 Cargo `exit_code: 101`。外层 300 秒上限在
  MINIBUILD 完成前终止 guest。
- `timeout 600s make run TARGET_ARCH=riscv64 > /tmp/buildstorm-map-stack-fork-riscv-release.log 2>&1`：
  release guest 同样完成 prepare 并进入 build，600 秒内没有输出 `BUILDSTORM_DEBUG_MINIBUILD
  fail`、panic、`TFAIL` 或 `TBROK`，但也没有到达 `BUILDSTORM_DEBUG_MINIBUILD ok` 或
  `shutdown!`；因此不能将 fresh MINIBUILD 标记为完整通过。

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

动态 `MAP_STACK` fork `EFAULT` 已修复。后续应以正式 `buildstorm_testcode.sh` 覆盖
`BUILDSTORM_MINIBUILD` 与 `BUILDSTORM_COMPILE`，不能将当前诊断脚本中跨过 EFAULT 的结果
等同于最终评分通过。另有一个独立的 mmap 记账问题待处理：`munmap()` 目前不会回收
`MAP_STACK` 的预算，且只处理完整覆盖的 VMA；它不应混入本修复。

## 用户态 `/tmp` 分阶段入口（2026-07-22）

上述内核注入方式用于最初将长测例快速拆开，但测试脚本文本属于用户态诊断载体，不应继续由
`create_init_files()` 在启动期写进 `/glibc`。本轮将全部 BuildStorm debug shell 正文迁至
`user/src/bin/buildstorm/`，内核删除对应常量、`create_buildstorm_debug_scripts()` 以及
`create_init_files()` 调用。这样普通根文件系统初始化不再带有 BuildStorm 专用测试资产。

`buildstorm::common` 在每个 case 运行前以
`openat(O_CREATE | O_WRONLY | O_TRUNC, 0o600)` 将脚本写到固定 `/tmp/buildstorm-*.sh`，循环
处理短写并检查 `close()`；写入成功后才通过现有 Bash runner 在 `/glibc` 工作目录执行。脚本仍
自行挂载 proc/sysfs/devtmpfs、导出 Rustup/Cargo 环境，尤其保留 MINIBUILD 的
`cargo build >/dev/null 2>&1` 和正式编译的 pipe/`tee` 拓扑，避免改变待诊断的 Rust/Cargo
行为。物化失败会输出 `BUILDSTORM_DEBUG_CASE ... stage=materialize`；执行失败则保留 child 的
wait status，`execve` 失败的 child 以 `127` 退出并表现为非零 wait status，不会再伪装成成功。

用户态模块提供以下独立入口：

| case | `/tmp` 脚本 | 覆盖范围与依赖 |
| --- | --- | --- |
| `toolchain` | `buildstorm-toolchain.sh` | `rustc --version` 与 `cargo --version` |
| `minibuild_prepare` | `buildstorm-minibuild-prepare.sh` | 删除并以 `cargo new` 创建 `/tmp/minibuild` |
| `minibuild_build` | `buildstorm-minibuild-build.sh` | 依赖 prepare，编译并运行 Hello World |
| `xtask_clean_target` | `buildstorm-xtask-clean-target.sh` | 对应参考脚本的交叉 target 清理 |
| `rename_publish` | `buildstorm-rename-publish.sh` | 独立验证临时 `.rmeta` rename 发布 |
| `xtask_prebuild` | `buildstorm-xtask-prebuild.sh` | 非计时 `cargo build -p tg-xtask` |
| `unicode_artifact` | `buildstorm-unicode-artifact.sh` | 依赖预构建产物，直接以 `rustc --extern` 验证 artifact |
| `xtask_build` | `buildstorm-xtask-build.sh` | 计时 `cargo xtask arceos build` 与产物大小检查 |

`SELECTED_CASE` 允许在一次启动中选择一项；`run_minibuild_fresh()` 固化
`prepare -> build` 依赖，`run_official_sequence()` 保留参考脚本主阶段顺序，
`run_diagnostics()` 额外执行 rename 和 unicode artifact 探针。所有局部路径继续只输出
`BUILDSTORM_DEBUG_*`，不得以正式 `BUILDSTORM_TOOLCHAIN`、`BUILDSTORM_MINIBUILD` 或
`BUILDSTORM_COMPILE` 标记替代，以免部分运行被 judge 误计为得分。

这次迁移只改善可定位性和运行入口，不重新证明此前的 mmap、fork、loader 或 rename 修复，也
不等同于完整 BuildStorm 或性能项通过；正式结论仍须分别运行原始 `buildstorm_testcode.sh`。

### 本轮迁移验证

`make build-arch TARGET_ARCH=riscv64` 与
`make build-arch TARGET_ARCH=loongarch64` 均通过。RISC-V 以 final-2026 原始镜像和
`-snapshot` 运行 120 秒，依次输出 `sigaltstack regression: PASS`、`rseq regression: PASS`、
`["/bin/bash\0", "/tmp/buildstorm-xtask-prebuild.sh\0"]` 和
`----- pre-build tg-xtask (untimed) -----`。这确认脚本由用户态写入 `/tmp` 后可被 Bash 读取和
执行；外层时限到期前未得到 prebuild 完成标记，因此不将该样本解释为 Cargo 或 BuildStorm 通过。

### 后续：全量正式评分入口（2026-07-22）

上文的 `run_official_sequence()` 在迁移当时确实只是 DEBUG-only 的诊断组合，不能作为
正式评分入口。后续发现 final-2026 已将全量路径接到该函数，导致 `log.ans` 即使出现
`BUILDSTORM_DEBUG_TOOLCHAIN ok` 和 `BUILDSTORM_DEBUG_MINIBUILD ok`，judge 仍为 0 分。

当前该函数已改为调用 `buildstorm::official::run()`：它在构建期以 `include_str!` 嵌入
`scripts/buildstorm_testcode.sh`，物化到 `/tmp/buildstorm-official.sh` 后由一个 Bash 进程
完整执行。分阶段与扩展诊断入口仍保留 `BUILDSTORM_DEBUG_*`，只有该全量入口产生正式的
`BUILDSTORM_TOOLCHAIN`、`BUILDSTORM_MINIBUILD` 和 `BUILDSTORM_COMPILE` 标记。此次接线
修复的独立复盘见 [buildstorm-full-run-marker-contract.md](./buildstorm-full-run-marker-contract.md)。
