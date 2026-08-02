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

`Ext4Inode::rename()` 当前按下列顺序处理普通路径 rename：

1. 源目录项仍存在时只先刷出 sparse ranges；dense byte-cache 暂不写回，避免每次 Rustc
   artifact 发布都提交完整文件。
2. 关闭仍使用旧路径的底层 descriptor，但使用不触发 mount-wide cache flush 的 helper。
3. 调用 `file_rename(path, new_path)`；错误直接映射为对应的 `SysErrNo`，不再统一伪装成
   `ENOENT`。
4. 只有 rename 成功后才调用 `rename_path_cache(path, new_path)`：在 `CACHE_TABLE` 中先移除
   覆盖目标，再把 source cache 放到新路径，并同步迁移 FIFO 项。rename 失败时 source cache
   保留，便于调用者重试。
5. 迁移后，`read_cached_at()` 为新路径的 read/read_all 和 mmap backing `read_at()` 提供 dirty
   bytes，lookup stat 通过 size/blocks overlay 反映缓存大小；随后切换 inode 内部 `Ext4File` 到
   `new_path`，更新 alias，并失效 source/destination 的 `FILE_PAGE_CACHE` 条目。

若 source 没有 dense cache，成功 rename 后会清理 source 与 destination 的孤立 pathname state；
若 source 有 cache，迁移完成后只清理 source key，不能再次丢弃新路径的 dirty cache。

同时，`write_back_cache_entry()` 现在把成功返回但字节数不足的写入记录为 `EIO`。此时
rename 前的 flush 失败，cache 不会被 discard，rename 也不会继续执行。

诊断脚本同步修正 native Cargo target 目录，加入实际的临时 `.rmeta` 写入、`mv` 和内容检查，
保留 Cargo 返回码，并输出 artifact 与 direct Rustc probe 信息，不再吞掉 `cargo build` 的
失败码。

## 当前边界

- 本修复覆盖成功的 pathname rename 发布链路，不扩展 lwext4 已有的“目标已存在时返回
  `EEXIST`”限制；Linux rename 覆盖语义仍需单独实现。
- dense cache 仍以 pathname 为索引，迁移只覆盖当前成功 rename 的 source/destination 两个 key；
  hard link 的其他 alias、目录后代和多 alias 同时写入仍需 inode 身份级 cache 才能完整覆盖。
- `ext4_rename_write_back` 现有计数器只统计显式 sparse/dense write-back/discard 阶段，尚未统计
  `rename_path_cache` 的迁移 ops/bytes。因此日志中 `ops=0` 不能证明没有发生 rename 或 cache 迁移。
- FIFO eviction 仍会先摘除 cache 再忽略 write-back error；本次短写检查保证 rename 的显式
  flush 不把短写当作完成，但不把 FIFO 的全局重试策略误报为已解决。

## 涉及文件

- `crates/lwext4_rust/src/file.rs`
- `os/src/fs/ext4_lw/inode/io.rs`
- `os/src/fs/ext4_lw/inode/namespace.rs`
- `os/src/fs/ext4_lw/inode/mod.rs`
- `os/src/utils/perf/report.rs`
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

## 后续测例拆分（2026-07-22）

为避免 rename 发布结果被宽泛的 `tg-xtask` 预构建输出掩盖，`rename_publish` 已成为独立的
用户态 BuildStorm case：它在运行时写入 `/tmp/buildstorm-rename-publish.sh`，单独创建临时
`.rmeta`、执行 `mv`，并验证旧路径不存在、最终路径保留 payload。`unicode_artifact` 同样从
预构建中拆出，要求在已有 `unicode_ident` artifact 后用 `rustc --extern` 单独验证可读性。

这只改变诊断入口和归因粒度，不改变本复盘中 rename/write-back 根因、修复顺序或历史验证结论；
独立 `rename_publish` 成功仍不能代替完整 Cargo、`E0463` 消失或正式 BuildStorm 通过证据。

## 当前实现复核（2026-08-02）

`tmp_08.ans` 在 Cargo `17/446` 附近再次出现 `unicode_ident`、`scopeguard` 的 `E0463`。该样本对应
的尝试跳过 dense cache 写回没有同时补齐 rename 后新路径的 read、mmap 和 stat 可见性，因此按正确性失败处理，
不作为性能样本。

随后实现了受限的 dense cache migration：sparse buffer 仍在 rename 前刷盘，dense cache 只在
`file_rename()` 成功后从 source key 迁移到 destination key；失败保留 source，目标残留先清理。`tmp_09.ans`
无 `E0463`、panic、`TFAIL` 或 `TBROK`，在 `t=568432ms` 到达 `33/446`，但没有完整 BuildStorm 结束标记。
当前结果只证明方向性正确性和阶段收益，仍需补齐 migration telemetry 及下列语义回归：rename 后
read/exec/stat/mmap、旧 fd 继写、失败回滚、覆盖目标、hard link、多 alias、`fsync/sync` 与
`unlink-open-close`。
