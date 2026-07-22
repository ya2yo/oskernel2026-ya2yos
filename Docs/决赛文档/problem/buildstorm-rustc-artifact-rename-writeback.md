# BuildStorm Rustc artifact rename 后 lwext4 write-back 缓存重建旧临时文件

## 背景

提交 `83c981c7073ffa9f8c6923db6c0ded9b7eae9f81` 已修复此前 Rustc 在
`unicode-ident/src/tables.rs` 报 `expected expression, found '='` 的
`mremap(MREMAP_MAYMOVE)` 数据丢失问题。

本次根目录 `log.ans` 暴露的是随后出现的不同症状：Cargo 编译 `proc-macro2` 时找不到
已经构建的 `unicode_ident` crate。这不是 `83c981c7` 的回归，也不是
`unicode-ident` 源码或宿主 Cargo 缓存损坏。

## 现象

关键错误为：

```text
error[E0463]: can't find crate for `unicode_ident`
error: could not compile `proc-macro2` (lib) due to 1 previous error
```

此前 `buildstorm_xtask_prebuild_debug.sh` 在 `cargo build` 后使用 `|| true`，所以即使
Cargo 失败也会打印 `BUILDSTORM_DEBUG_XTASK_PREBUILD done`，该标记不能证明构建成功。

## 分析

Rustc/Cargo 发布 `.rmeta`、`.rlib` 时会先将内容写入临时路径，再用 rename 原子发布到
最终 artifact 路径。lwext4 的小文件 write-back cache 则以 pathname 为键，状态存放在
`CACHE_TABLE` 和 `FIFO_TABLE`。

旧的 `Ext4Inode::rename()` 先执行 `file_rename(old_path, new_path)`，随后才关闭仍记录
`old_path` 的 `Ext4File`。`file_close()` 会调用 `file_cache_flush()`；若旧路径的脏 cache
仍然存在，此时目录项已经移走，`write_back_cache_entry()` 以 `O_RDWR` 打开旧路径会得到
`ENOENT`，随后会用 `O_CREAT | O_TRUNC` 重建该旧临时文件并写回。

这样临时 artifact 的内容可能在 rename 前尚未完整落盘，而 rename 后又有旧 pathname 的
回写。最终路径因此可能保留空、旧或不完整内容，后续 Rustc 以
`--extern unicode_ident=...` 使用它时报告 `E0463`。

审查还发现 `write_back_cache_entry()` 过去只检查 `ext4_fwrite()` 的返回码，没有确认
`rw_count == cache_writer.size`。底层 lwext4 允许在已写入部分数据后返回短写，因此 rename
前的“flush 后丢 cache”必须把短写视为失败，不能把未完整持久化的数据误删。

## 根因

VFS rename 的目录项变化顺序与 pathname-keyed write-back cache 的生命周期不一致：旧路径
在目录项消失后仍可被 close 或 FIFO eviction 回写。该 cache 还没有以稳定 inode 身份追踪
rename，因此单独失效 `FILE_PAGE_CACHE` 不能清除 lwext4 的 `CACHE_TABLE` / `FIFO_TABLE`。

## 修复

`Ext4Inode::rename()` 现在按下列顺序处理普通路径 rename：

1. 源目录项仍存在时，调用 `flush_and_discard_path_cache()`，先完整写回 active pathname 的
   cache，再从 `CACHE_TABLE` 和 `FIFO_TABLE` 移除它。
2. 关闭仍使用旧路径的底层 descriptor。
3. 调用 `file_rename(path, new_path)`；错误直接映射为对应的 `SysErrNo`，不再统一伪装成
   `ENOENT`。
4. 仅当 rename 成功后，通过 `discard_path_cache()` 丢弃 source 与 destination 的孤立
   write-back state。这样既不会重建旧临时名，也不会让 destination 的历史残留状态覆盖新
   artifact；rename 失败时不会错误丢弃目标路径数据。
5. 切换 inode 内部 `Ext4File` 到 `new_path`，更新 alias，并失效 active/source/destination
   的 `FILE_PAGE_CACHE` 条目。

同时，`write_back_cache_entry()` 现在把成功返回但字节数不足的写入记录为 `EIO`。此时
rename 前的 flush 失败，cache 不会被 discard，rename 也不会继续执行。

诊断脚本同步修正 native Cargo target 目录，加入实际的临时 `.rmeta` 写入、`mv` 和内容检查，
保留 Cargo 返回码，并输出 artifact 与 direct Rustc probe 信息，不再吞掉 `cargo build` 的
失败码。

## 当前边界

- 本修复覆盖成功的 pathname rename 发布链路，不扩展 lwext4 已有的“目标已存在时返回
  `EEXIST`”限制；Linux rename 覆盖语义仍需单独实现。
- 路径级 `FILE_PAGE_CACHE` 对目录后代、hard link 其他 alias 和 `MAP_SHARED` rename 的完整
  一致性仍是既有设计边界。
- FIFO eviction 仍会先摘除 cache 再忽略 write-back error；本次短写检查保证 rename 的显式
  flush 不把短写当作完成，但不把 FIFO 的全局重试策略误报为已解决。

## 涉及文件

- `crates/lwext4_rust/src/file.rs`
- `os/src/fs/ext4_lw/inode.rs`
- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `Docs/决赛文档/problem/buildstorm-rustc-artifact-rename-writeback.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

已通过：

```text
git diff --check
make build-arch TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
```

RISC-V QEMU 快速回归执行了真实的临时 artifact 写入和 rename，输出：

```text
sigaltstack regression: PASS
rseq regression: PASS
BUILDSTORM_DEBUG_XTASK_PREBUILD rename_publish=PASS
```

该 probe 已确认旧 pathname 未被重新创建、最终 pathname 保留预期 payload。之后 guest 进入
`cargo build -p tg-xtask`，但本轮未得到脚本的 `cargo_rc`、artifact probe 或完整 BuildStorm
结束标记。此前一次 `timeout 900s make run TARGET_ARCH=riscv64` 也在 Cargo 仍运行时被外层
时限终止。因此本修复不将完整 Cargo、`unicode_ident` 的 `E0463` 消失或完整 BuildStorm 标记为
已通过。
