# Linux EXT4 锁模型与 lwext4 admission

## 背景

`tmp_13.ans` 显示 BuildStorm 仍在 `EXT4_OP_LOCK` 排队。维护者要求按 Linux EXT4 的方式拆分，而不是保留一个覆盖
全部 lwext4 调用的全局锁。

## Linux 7.0 对照

Linux 没有 Ya2yOS 原先这种“所有 EXT4 path/file API 都必须持有同一 mount gate”的锁。它按资源分层：

- `fs/ext4/file.c:79` 的 direct read 以 `inode_lock_shared()` 保护单 inode；写入按是否扩展/分配选择 shared 或
  exclusive inode lock。
- `fs/ext4/ext4.h:1052` 明确说明 xattr 读写使用 `xattr_sem`，避免和普通文件数据的 `i_rwsem` 互相串行；
  `i_data_sem` 用于 truncate 与 block mapping 的竞态。
- `fs/namei.c:2924`、`fs/namei.c:3732` 及附近按父目录、源 inode、目标 inode 的顺序取得 `i_rwsem`，而不是锁住整个
  superblock 的每次路径访问。

这说明目标应是分层锁，而不是坚持 mount-wide reader mutex；它不证明 lwext4 可以立刻无锁并发。lwext4 没有 Linux
的 VFS inode、目录锁、block-group allocator 锁和 JBD2 handle，因此直接删除 gate 会让这些尚未保护的写侧状态并发。

## P21.3 shared admission（已撤回）的实现

`EXT4_OP_LOCK` 改为 task-aware FIFO reader/writer admission。连续的队首 shared waiter 可以一批进入；一旦 writer
排队，后来的 reader 不能插队。每 inode 现有 `write_state -> io_state -> EXT4_OP_LOCK` 顺序不变，任务退出会回收其
writer 或 reader admission，避免永久阻塞。

本轮开放的 shared 调用是已审计的纯读 C 路径：已打开 descriptor 的 `read-data`、find/path stat、directory iteration、
readlink、get/listxattr、link-count、`statfs` 和 root directory listing。namespace、写入、sync、close、seek、metadata
setter，以及 descriptor open/rebind 仍 exclusive。

`fstat` 和 `fmode` 也维持 exclusive。前者会先执行 sparse write buffer flush 以保证 `st_size/st_blocks` 可见，后者在
失败恢复时会经 `recover_live_path()` 调用 `file_close()`；两者都可能写回，不能伪装为 shared read。

## `tmp_13.ans` 证据

RISC-V 8 HART 日志有 `BUILDSTORM_TOOLCHAIN/MINIBUILD ok`，直到 Cargo `Building 11/446` 未见 panic、SIGSEGV、
`ERROR`、`TFAIL` 或 `TBROK`，但没有 `BUILDSTORM_COMPILE ... ok=true`、测试组 `END` 或 `shutdown!`。

最后累计 gate 统计为 `shared_acquires=39042`、`shared_handoffs=2224`、`max_active_readers=7`，证明 shared batch 已实际
并发。累计 handoff wait 仍是 `567915801us`，并且 `fstat` sparse flush 为 6，因此当前样本只能证明阶段稳定与方向，
不能报告端到端性能提升。

## 2026-08-03：`server.ans` / `client.ans` 死锁与回退

### 现象

新的 `server.ans` 在 `BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok` 之后的
`cargo build -p tg-xtask` 预构建阶段永久停住；没有 panic、`TFAIL` 或 `TBROK`。维护者随后在同一
RISC-V 8 HART QEMU 上中断并导出 `client.ans` 的全 CPU backtrace：CPU 0、2、3、7 都在
`ext4_fs_rwlock_write_lock()`（`ext4_fs.h:105`）的 C 自旋循环，调用方分别是
`ext4_fopen2_with_metadata()` 和 `ext4_dir_mk_exclusive_with_metadata()`；另外四个 hart 已处于
`wfi()` idle。

等待者对应的是 Cargo 同时创建的 `target/debug/.fingerprint/*`、`target/debug/build/*/rustc*` 和
`target/debug/deps/*.o`。没有一个 CPU 停在已取得 `namespace_lock` 的 C 临界区，因此这不是普通
`EXT4_NS_WRITE_UNLOCK` 漏调。

### 根因

提交 `3ec47ec1` 删除 Rust 的 `EXT4_OP_LOCK` 后，让多个可调度 Ya2yOS task 直接进入
`struct ext4_fs_rwlock`。该锁使用 `__atomic_compare_exchange_n()` 和无限 busy loop，不会调用
内核的 wait/wake 或 scheduler：

1. 任务 A 取得 `namespace_lock.state = -1` 后，在 C 临界区被 timer 抢占；
2. 其它 worker 随后在同一或其它 hart 进入 C write lock 并无限自旋；
3. 自旋任务不让出 CPU，任务 A 虽然 runnable 却无法再次获调度；
4. 所有可运行的 Cargo worker 最终陷入自旋，未参与争用的 hart 进入 idle。

这正好解释了 `client.ans` 的“四个 C writer spinner + 四个 idle hart + 无持锁栈”组合。即使是 C
read lock 也会在 writer 活跃时裸自旋，故不能只恢复写路径 gate 而继续开放所谓 shared read admission。

### 修复

- `os/src/fs/ext4_lw/mod.rs` 恢复 `Ext4OpLock`，但不重新引入已经移除的 perf 分类。新实现是 task-aware、FIFO、**排他**
  的挂载级准入：竞争 task 通过 `block_on` 睡眠，只有不带 current task 的启动上下文才使用 spin fallback。
- 历史上受 admission 覆盖的所有 lwext4 C API 入口重新取得 `EXT4_OP_LOCK`，包括 create/mkdir/rename/link/unlink、
  open/read/write/truncate/sync/seek、path lookup、metadata/xattr/readdir、inode close/drop 和 superblock
  `statfs/sync/ls`。因此同一挂载内不会有两个 Ya2yOS task 同时进入 C raw rwlock。
- `os/src/task/mod.rs` 在发散式 task exit 前调用 `cancel_ext4_op_waiter()`：被杀死的 waiter 无法析构其 future，必须移除
  FIFO ticket；若 owner 在 C callback 的可阻塞点退出，也回收逻辑 owner，保持旧 gate 的退出清理语义。
- 独立修复 `ext4_trunc_inode()` 的嵌套 transaction 自锁。无 on-disk journal 时旧条件
  `jbd_journal && curr_trans` 会误判外层 transaction 不存在，进而第二次获取已持有的 `journal_lock`；现在以实际的
  `journal_lock_held` 判断并临时切分/恢复 transaction scope。

这个修复以正确性优先，暂时牺牲 P21.3 的 C API 并发收益。P21.3 的 shared admission 不再是可用实现；未来只有把
C 锁替换为可等待的内核回调、或形成明确的不可抢占临界区及完整锁序证明后，才能重新开放细粒度并发。

## 验证

本轮 `cargo fmt --manifest-path os/Cargo.toml --all -- --check`、`git diff --check` 与
`make perf TARGET_ARCH=riscv64` 通过。完整 BuildStorm、`e2fsck -fn` 和 LoongArch64 构建/运行仍须补做；维护者已说明
当前 EXT4 全量回归超过一小时，未将截断样本误记为完整通过。

本次回退后的 `cargo fmt --manifest-path os/Cargo.toml --all -- --check`、`git diff --check`、
`make TARGET_ARCH=riscv64` 与 `make TARGET_ARCH=loongarch64` 均通过。直接以默认 `-m 16G -smp 8` 启动新
`kernel-rv`（基础镜像保持 `-snapshot`）完成引导、两个 signal regression 并进入 BuildStorm 的 `tg-xtask` 预构建；
在 360 秒主动 timeout 前，Cargo 已从旧死锁样本附近的 `2/446` 推进至 `15/446`，日志无 panic、`TFAIL`、`TBROK` 或
`ERROR`，并由 timeout 输出 `qemu-system-riscv64: terminating on signal 15`。这证明它跨过了本次已知死锁点，但没有
`BUILDSTORM_COMPILE ... ok=true`、测试组 `END` 或 `shutdown!`，不能报告完整通过。额外的 `-m 1G -smp 2` 尝试在
CMA 早期初始化后无进一步输出且被 300 秒 timeout 停止；它不符合内核固定 16 GiB memory layout，不能作为文件系统
回归。当前仍没有完整 BuildStorm、`e2fsck -fn` 或运行时 LoongArch64 通过结论。

## 证据索引

- `fs/ext4/file.c:79`：direct I/O read 的 shared inode lock。
- `fs/ext4/ext4.h:1052`、`fs/ext4/ext4.h:1121`：xattr 与 extent/truncate 的独立锁域。
- `fs/namei.c:2924`、`fs/namei.c:3732`：目录和 rename 的 inode 锁顺序。
- `os/src/fs/ext4_lw/mod.rs`：FIFO reader/writer admission 与退出清理。
- `os/src/fs/ext4_lw/inode/metadata.rs`：fstat recovery 与 xattr 锁分类。
- `tmp_13.ans`：8 HART 阶段 telemetry。

## 未验证点

- lwext4 的 allocator、目录修改和 journal 还没有 Linux 等价的细粒度锁，不能开放不同 inode 写入并发。
- 需要同一镜像、QEMU `-smp`、Cargo jobs 和冷启动下至少两次完整 A/B，才能量化收益。
- shared C 路径仍需 RISC-V/LoongArch64 压力、`e2fsck -fn` 与相关 LTP 文件语义回归。
