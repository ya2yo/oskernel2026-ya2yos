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

## 实现

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

## 验证

本轮 `cargo fmt --manifest-path os/Cargo.toml --all -- --check`、`git diff --check` 与
`make perf TARGET_ARCH=riscv64` 通过。完整 BuildStorm、`e2fsck -fn` 和 LoongArch64 构建/运行仍须补做；维护者已说明
当前 EXT4 全量回归超过一小时，未将截断样本误记为完整通过。

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
