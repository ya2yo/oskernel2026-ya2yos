# BuildStorm EXT4 资源锁分类观测与已解析 fd 锁域收缩

## 背景

lwext4 已从挂载级 `EXT4_OP_LOCK` 迁移到 namespace、inode/group stripe、super、journal 和 cache 等资源锁，并由 Rust
侧 task-aware `TaskRwLock` 实现可睡眠等待。此前 perf 只汇总全部资源锁，无法区分 pathname namespace 争用、inode 数据
访问、journal 或 cache 写回；同时若干已解析文件描述符的操作仍持有 namespace 读锁，放大了目录修改与数据 I/O 的互相阻塞。

## 现象

`tmp_09.ans` 的资源锁统计显示 namespace 是当前可测的主要争用域：累计等待 `1,141,136,319us`、竞争 `4,515` 次，
最大单次等待 `121,672,904us`。最后一个 `t=514315ms`、持续 `168.879s` 的 interval 单独有
`679,046,748us` namespace wait。该数据说明继续笼统拆锁没有依据，必须先按资源类别校验锁域。

该日志只覆盖到 BuildStorm 的中间编译阶段，不包含完整 `BUILDSTORM_COMPILE ... ok=true` 或测试组结束标记，不能用于端到端速度结论。

## 分析

对 C API 语义逐项区分：

- `fread`、`fwrite`、`ftruncate` 与 `SEEK_DATA/SEEK_HOLE` 已由 `ext4_file` 固定目标 inode，不再需要 namespace 锁保护 pathname 解析；
- `fopen`、`fopen2` 和 `fopen2_with_metadata` 的纯只读分支只解析路径，不应进入 mount-wide 的 delayed write-back 嵌套；
- `cache_write_back` 的计数更新很短，但计数归零后的脏块 flush 可能很长，二者共用 cache state lock 会无谓阻塞其它元数据状态访问。

这些收缩依赖现有 VFS 的末链 unlink 延迟语义：打开 fd 仍引用目标 inode，故不将已解析 fd 的数据操作重新绑定到 pathname。创建、截断、目录项变更及其它 pathname 操作仍保持 namespace 写锁或相应元数据锁。

## 修改

1. 在 lwext4 C `ext4_fs_rwlock` 中标记 namespace、inode、group、super、journal、cache state 与 cache flush 类别；Rust hook 建立资源锁时读取类别，并在 perf 中分别累计 acquire、排队、wait、hold、队深和 registry 开销。
2. 新增独立 `cache_flush_lock`：短暂更新 `cache_write_back` state 后释放 `cache_lock`，仅在计数归零的实际 flush 期间持有 flush lock，防止下一写回域与该 drain 交叠。
3. 对已解析 fd 的 `fread`、`fwrite`、`ftruncate`、`SEEK_DATA/SEEK_HOLE` 删除 namespace 读锁，只保留 inode stripe；纯只读 open 不再启动 write-back scope。可能创建或截断的 open 分支维持原有写回和 namespace 写锁。
4. Rust 解锁回调改为只查询已登记资源，避免 unlock 路径再次走会创建条目的登记逻辑。

## 涉及文件

- `crates/lwext4_rust/c/lwext4/include/ext4_fs.h`
- `crates/lwext4_rust/c/lwext4/src/ext4.c`
- `crates/lwext4_rust/c/lwext4/src/ext4_blockdev.c`
- `crates/lwext4_rust/c/lwext4/src/ext4_fs.c`
- `os/src/fs/ext4_lw/mod.rs`
- `os/src/utils/perf/fs.rs`
- `os/src/utils/perf/report.rs`

## 验证

- staged C/Rust 改动已通过 `cargo fmt --manifest-path os/Cargo.toml --all -- --check` 与 `git diff --check`。
- 本机 `make perf TARGET_ARCH=riscv64` 在 lwext4 CMake 配置时因缺少 `riscv64-linux-musl-cc` 中止，未得到 guest 运行结果。
- 尚未完成该版本的 BuildStorm、`e2fsck -fn`、文件系统 LTP 或 LoongArch64 运行回归；因此锁域变化和 cache flush 分离仅记录为待验证优化，不报告性能收益或跨架构正确性。
