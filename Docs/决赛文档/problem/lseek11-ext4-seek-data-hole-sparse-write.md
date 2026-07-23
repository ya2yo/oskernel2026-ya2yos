# LTP lseek11 ext4 SEEK_DATA/SEEK_HOLE 与稀疏文件语义修复

## 背景

Linux 的 `lseek(2)` 除 `SEEK_SET`、`SEEK_CUR`、`SEEK_END` 外，还定义了
`SEEK_DATA = 3` 和 `SEEK_HOLE = 4`。它们依赖文件系统对已分配数据块与稀疏洞的
区分：从 data/hole 内部开始应返回原 offset，跨洞时应返回下一个已分配块起点；
`SEEK_DATA` 在 EOF 后或后方不存在数据时必须返回 `ENXIO`，`SEEK_HOLE` 在最后一个
已分配块之后返回 EOF。

LTP `lseek11` 会先探测实际分配粒度，再将文件截断为 30 个块，分别在逻辑块 0、10、20
和末尾写入短数据，以验证三个洞、四段数据及 EOF 隐式洞。该用例同时覆盖 sparse write、
partial write、`fsync`、`ftruncate` grow 和两种 seek 操作。

## 现象

初始 `log.ans` 中 musl/glibc 的 `lseek11` 都不能正确实现 `SEEK_DATA`/`SEEK_HOLE`。
接入 extent 查找后，定位结果已正确，但当前源码重新构建后的日志仍出现相同的两项数据
读取失败：

```text
lseek11.c:204: TFAIL: The 0th test failed:
SEEK_DATA from startblock 0 offset 0, expect 'data01' return ''
lseek11.c:204: TFAIL: The 1th test failed:
SEEK_DATA from startblock 0 offset 4, expect '01suffix' return ''
```

为隔离 EOF 扩展语义，临时运行同一镜像中的 `ftruncate01`。两种 libc 都在
`ftruncate(256 -> 512)` 后读到旧字节 `97` (`'a'`)，而不是零：

```text
ftruncate01.c:53: TFAIL: ftruncate() got incorrect data 97, expected 0
```

这说明问题不再是 seek 映射，而是 ext4 数据块内容与逻辑长度之间失去一致性。

## 分析

`lseek11.c` 的分配粒度探测最后会在 offset `4094` 写入一个字节，因此 grow 前文件大小为
`4095`，逻辑块 0 已分配。随后测试把文件扩展到 `30 * 4096`，必须将原 EOF 到块尾的
`[4095, 4096)` 清零，之后才在块 0 开头写入 `data01suffix`。

修复前后的链路分别存在以下缺口：

1. `OSFile::lseek()` 仅接受 whence `0..=2`，VFS inode trait 和 lwext4 wrapper 没有
   `SEEK_DATA`/`SEEK_HOLE` 扩展点。
2. lwext4 对空 inode 的查找过早返回 hole，写侧无法进入 extent mapper；EOF 后的 sparse
   write 又使用按旧 EOF 推导逻辑块的 append helper，可能将写入映射到错误的 logical block。
3. Rust whole-file write-back cache 只保存字节，不保存 extent 分配状态。若 sparse inode
   继续使用该 cache，回写零填充字节会把洞物化为实际数据块。
4. 新数据 extent 和 `ftruncate` 保留的半块尾部必须清零，但普通文件 payload 的
   `ext4_fread()`/`ext4_fwrite()` 使用 direct block I/O。旧的 tail-zero helper 将整块数据
   放入 bcache 并标记为 dirty；后续 direct partial write 不更新该缓存副本，后续 flush
   可以把旧快照写回设备。于是 extent 仍显示块 0 是 data，而读到的内容变成全零或旧字节。
5. C 源和头文件变化曾不能可靠触发 lwext4 静态库重建，且不同 target 共用 build 目录，
   容易让 QEMU 使用过期或错误架构的 C 对象。

## 根因

根因是 ext4 稀疏文件语义跨越 VFS、Rust cache 和 lwext4 C 数据路径时不完整：一方面缺少
基于 extent 映射的 `SEEK_DATA`/`SEEK_HOLE`，另一方面在 grow/shrink partial block 上同时
存在 bcache 数据副本和 direct payload I/O。后者破坏了 `ftruncate` 新暴露范围必须为零的
Linux 语义，并会覆盖随后合法的小块写入，直接造成 `lseek11` 的 block 0 数据为空。

## 修复

### VFS 与 Linux ABI

- 在 `fs` 中定义 Linux `SEEK_DATA`/`SEEK_HOLE` 常量，并为 `Inode` 增加 `seek_data()`、
  `seek_hole()` 扩展点。
- `OSFile::lseek()` 对 `SEEK_SET/CUR/END` 使用 checked arithmetic；对 data/hole 拒绝负
  offset，按 Linux 规则处理 EOF/`ENXIO`，成功后更新 open file description 的共享 offset。
- lwext4 增加受 mount lock 保护的 extent 查询 FFI。它逐 logical block 查询映射，data/hole
  使用 physical block 是否为零判断，并在最后一个已分配块后的 EOF 提供隐式 hole。

### 稀疏 extent 与缓存

- 空文件仅在读侧查询时直接作为 hole；写侧会进入 extent mapper 分配首块。
- sparse write 始终按调用方的目标 logical block 分配，而不是让 append helper 从旧 EOF
  重算位置；无法获得物理块时返回 `EIO`，绝不向 physical block 0 写入。
- hole/unwritten block 的读取直接填零。新分配的 extent 在发布为 initialized extent 前用
  direct I/O 清零，避免暴露已释放 inode 的残留内容。
- whole-file cache 改为按 `(mountpoint, inode)` 禁用。发生 truncate、稀疏写或跨 alias 的
  hole 操作前会回写并移除同一 inode 的路径 cache，避免 byte-only cache 将洞回写成 data；
  对已有 sparse inode 的首次 cacheable 打开也会执行同一 flush-then-discard 状态机；大于
  `MAX_CACHED_FILE_SIZE` 的文件本来不进入 byte-only cache，不再为检测洞而线性扫描整个
  allocation map。`O_TRUNC` 会丢弃截断前的 inode cache，删除最后一个 link 时则临时查询
  目标 inode 后清理该 policy，避免 inode 复用后错误禁用新文件的 cache。
- `check_cached()` 只在完整 `ext4_fread()` 成功后注册 cache；FIFO 淘汰在写回成功后才移除
  entry，写回或 unlink 失败时保留脏 cache 以供重试。

### ftruncate 尾部清零

- `ftruncate` grow 走 `ext4_fs_truncate_inode()`，只更新逻辑长度，不为洞分配块；shrink 和
  grow 都对仍映射的 EOF 所在 partial block 清除新不可见或新可见的尾部范围。
- `ext4_fs_zero_inode_tail()` 不再通过 `ext4_trans_block_get()` 把 regular-file data block
  放入 bcache。它分配短暂零缓冲区并调用 `ext4_block_writebytes()`，由底层扇区
  read-modify-write 保留 block prefix。数据清零与普通 payload 同走 direct I/O，inode size 只在清零成功
  后更新，因而既没有 stale bcache 覆盖，也不会提前暴露未清零的字节。
- `ext4_ftruncate_no_lock()` 的 inode-ref 获取失败统一交由外层 `ext4_ftruncate()` 解锁 mount，
  避免错误路径重复释放 mount lock；cleanup 错误只在主操作成功时才覆盖返回值。

### 构建可靠性

- `build.rs` 和 lwext4 `Makefile` 跟踪 C 源/头变更，并按 Rust target 分离 lwext4 build
  目录，确保 LoongArch64/RISC-V 都重新链接对应的 C 实现。

## 涉及文件

| 文件 | 修改 |
|---|---|
| `os/src/fs/mod.rs` | 定义 Linux seek ABI 常量。 |
| `os/src/fs/vfs.rs` | 增加 inode data/hole 查询扩展点。 |
| `os/src/fs/files/os_file.rs` | 分发四类 lseek 语义并更新 OFD offset。 |
| `os/src/fs/ext4_lw/inode.rs` | 将 VFS 查询、sparse write 与 truncate 接入 lwext4。 |
| `crates/lwext4_rust/src/file.rs` | 暴露 FFI，并按 inode 管理 whole-file cache policy。 |
| `crates/lwext4_rust/c/lwext4/include/ext4.h` | 声明 data/hole seek C API。 |
| `crates/lwext4_rust/c/lwext4/src/ext4.c` | 实现 extent seek、sparse write 和 ftruncate 调度。 |
| `crates/lwext4_rust/c/lwext4/src/ext4_extent.c` | 在发布新数据 extent 前 direct-zero。 |
| `crates/lwext4_rust/c/lwext4/src/ext4_fs.c` | 修正空 inode 写侧映射和 truncate partial-tail 清零路径。 |
| `crates/lwext4_rust/build.rs`、`c/lwext4/Makefile` | C rebuild 依赖与按 target 隔离的构建目录。 |

## 验证

已执行：

```text
git diff --check
cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml -- --check
docker run ... 'make build-arch TARGET_ARCH=loongarch64'
docker run ... 'timeout 120s make run TARGET_ARCH=loongarch64 > log.ans 2>&1'
docker run ... 'make build-arch TARGET_ARCH=riscv64'
docker exec ... 'make build-arch TARGET_ARCH=loongarch64'
docker exec ... 'timeout 180s make run TARGET_ARCH=loongarch64 > log.ans 2>&1'
docker exec ... 'make build-arch TARGET_ARCH=riscv64'
```

在移除临时测试入口前，LoongArch64 的 musl/glibc `ftruncate01` 均为
`passed 2 failed 0 broken 0`。最终入口只保留维护者原有的 `lseek11`；在缓存状态机和
`ftruncate` 锁错误路径收口后重新构建并运行，最新 `log.ans` 中两种 libc 均为：

```text
lseek11: passed 15 failed 0 broken 0
shutdown!
```

LoongArch64 runtime 使用维护者选择的 pre-tests 镜像完成。RISC-V 在最终 cache/lock 补丁后
再次完成 release 构建，但未启动其默认 `8G/8 CPU` final-2026 QEMU 配置；因此不将 RISC-V
runtime 语义标记为已验证。
