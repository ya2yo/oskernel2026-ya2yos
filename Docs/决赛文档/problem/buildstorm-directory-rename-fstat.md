# BuildStorm 父目录 rename 后打开子文件 fstat 返回 ENOENT

## 现象

`server.ans` 在 BuildStorm 编译 `axbuild` 时出现：

```text
[ERROR] [HART1] [PID 1119] [TID 1121] ext4_stat_get: rc = 2
[WARN] [HART1] [PID 1119] [TID 1121] Ext4Inode::fstat: ext4_stat_get failed rc=2, path="/work/tgoskits/target/debug/incremental/axbuild-3n5p0sz3bjvhw/s-hi3rnh9l4s-0no8a9z-working/query-cache.bin"
```

lwext4 的 `rc = 2` 对应 `ENOENT`，表示按旧路径找不到目录项，不表示 EXT4 块设备或
inode 元数据已经损坏。这里的文件仍然由 rustc 打开，失败发生在父目录被整体重命名之后。

## 触发时序

Rustc 发布增量缓存时会先替换临时文件，再把 working 目录整体改名，典型操作为：

```text
rename(".../s-hi3rnh9l4s-0no8a9z-working/dep-graph.part.bin",
       ".../s-hi3rnh9l4s-0no8a9z-working/dep-graph.bin")
rename(".../s-hi3rnh9l4s-0no8a9z-working",
       ".../s-hi3rnh9l4s-0no8a9z-working-final-hash")
```

第二个 `rename` 只改变目录项路径，但 Ya2yOS 原来的 lwext4 wrapper 和 VFS 索引主要以
字符串路径定位对象。父目录改名后，已经打开的 `query-cache.bin` 仍保存
`...-working/query-cache.bin`，后续 `fstat()` 继续调用旧路径的 `ext4_stat_get()`，于是
收到 `ENOENT`。

## 根因

原目录 rename 路径只更新被重命名的目录 inode，并清理 `FsIndex` 的旧/新根路径；没有把
旧目录下已经缓存或打开的后代 inode 一起重定位。因此出现三层状态不一致：

1. lwext4 `Ext4File.file_path` 仍是旧前缀；
2. `Ext4Inode` 的 live path、alias 和 dirty dense write-back cache 仍引用旧前缀；
3. `FsIndex::INODE_CACHE` 仍把后代 inode 绑定到旧路径。

这与 [BuildStorm 跨进程 unlink 后 fstat 返回 ENOENT](./buildstorm-cross-process-unlink-fstat.md)
不同：unlink 问题是目录项被删除但其他进程仍持有同一 inode，本次问题是目录项仍存在、
但整个父目录的路径前缀发生了变化。前者需要延迟删除，后者需要在 rename 成功后迁移
路径状态。

## 修复

- 在 `crates/lwext4_rust/src/file.rs` 增加 `Ext4File::remap_path()`，只更新 Rust wrapper
  保存的路径，不重复调用底层 rename；底层 `ext4_file` 继续代表同一个 inode。
- 在 `os/src/fs/vfs.rs` 的 `Inode` trait 增加默认的 `remap_path_prefix()`，并由
  `os/src/fs/ext4_lw/inode/vfs.rs` 转发到 EXT4 inode 实现。
- 在 `os/src/fs/ext4_lw/inode/mod.rs` 按既有 `write_state -> io_state` 锁序迁移 live path、
  aliases、dirty dense write-back cache、`FILE_PAGE_CACHE` 路径和 `Ext4File.file_path`，
  同时更新 VFS path mirror。
- 在 `os/src/fs/kernel_fs_ops/fsidx.rs` 增加 `FsIndex::remap_subtree_paths()`：原子迁移旧
  前缀及全部后代路径，清理目标子树冲突项，释放索引锁后逐个通知存活 inode，并同步迁移
  special-node 类型。
- 在 `os/src/syscall/fs/ctl/namespace.rs` 中，目录 `renameat2()` 成功后调用
  `remap_subtree_paths()`；普通文件仍沿用原来的单 inode cache 清理路径。
- `fstat_with_stage_observer()` 对可由 alias 恢复的第一次旧路径 `ENOENT` 降为带路径的
  `debug!`，最终恢复失败仍由 `Ext4Inode::fstat` 输出 `WARN`，避免正常 rename 恢复过程
  产生误导性的 ERROR。

## 回归测试

新增 `user/src/bin/initproc/fstat_rename_subtree_regression.rs`：创建目录并打开
`query-cache.bin`，写入 4 字节后将父目录改名，最后通过原始 fd 调用 `fstat()`，校验 inode
有效且大小仍为 4。该测例接入 `test_final_2026()`，不会预先删除不存在的路径，避免把
测试自身的清理动作混入日志。

## 验证

- `make TARGET_ARCH=riscv64`：根 Makefile 的 RISC-V64 与 LoongArch64 release 构建均通过。
- `make TARGET_ARCH=riscv64 run`：输出 `fstat rename subtree regression: PASS`；CAgent 全部
  通过，BuildStorm 输出 `BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok` 并进入
  `pre-build tg-xtask (untimed)`。
- 本次运行未再出现 `ext4_stat_get: rc = 2` 的 ERROR 或 `Ext4Inode::fstat` WARN，且 QEMU、
  `make`、`cargo`、`rustc` 进程均已正常结束。
- 完整 BuildStorm 在本轮尚未跑完，只确认到 pre-build 阶段；因此不将完整评分结果记为已验证。

## 涉及文件

- `crates/lwext4_rust/src/file.rs`
- `os/src/fs/vfs.rs`
- `os/src/fs/ext4_lw/inode/mod.rs`
- `os/src/fs/ext4_lw/inode/vfs.rs`
- `os/src/fs/kernel_fs_ops/fsidx.rs`
- `os/src/syscall/fs/ctl/namespace.rs`
- `user/src/bin/initproc.rs`
- `user/src/bin/initproc/fstat_rename_subtree_regression.rs`

当前工作区未提交；建议提交信息：`fix(fs): preserve open child paths across directory rename`。
