# BuildStorm 增量缓存发布缺少 rename 覆盖语义

## 背景

Cargo/rustc 使用临时文件加 `rename()` 原子发布 dep-graph 和编译 artifact。Linux `rename()` 在目标为
已存在普通文件时应替换目标，而不是返回 `EEXIST`。

## 现象

旧运行在 Cargo 约 `440/446` 附近出现：

```text
ext4_frename error: rc = 17
failed to move dependency graph ... File exists
could not compile ...
```

失败后，持久化镜像中留下若干 0 字节 `dep-graph.bin`。新内核再次读取这些旧缓存时会先输出：

```text
warning: could not load dep-graph ...: memory map must have a non-zero length
```

该 warning 表示 Cargo 丢弃损坏的增量图并重新编译，不是新的内核 panic。

## 分析

`ext4_frename()` 原实现直接调用 `ext4_create_hardlink(new_path, ..., true)`。目标路径已存在时，
hardlink 创建返回 `EEXIST`，导致 Cargo 无法用新 dep-graph 覆盖旧文件。此前 pathname write-back
cache 的迁移修复保证了 rename 前后内容可见性，但没有实现目标替换语义。

最新 `server.ans` 中上述 0 字节 warning 出现后，Cargo 从 `440/446` 继续推进到 `444/446`；日志中
不再有 `ext4_frename ... rc = 17`、`failed to move dependency graph ... File exists` 或
`could not compile`，说明发布路径已能覆盖旧目标，遗留缓存正在被重建。

## 根因

lwext4 的 `ext4_frename()` 只支持目标不存在的 hardlink 加 unlink 流程，缺少 Linux 对已存在目标
普通文件的替换语义。

## 修复

在同一个 namespace 写锁和 journal transaction 内，rename 创建新目录项前先处理目标：

- 目标不存在时沿用原流程；
- 源和目标是同一 inode 时按 no-op 成功返回；
- 源和目标类型不匹配时返回 `EISDIR` 或 `ENOTDIR`；
- 目标是普通文件时，按最后链接处理数据块、删除目标目录项并在链接数归零时释放 inode；
- 随后创建指向源 inode 的目标 hardlink，并删除源目录项。

当前仍不支持以目录覆盖目录；该路径继续返回 `EEXIST`，避免在没有补齐父目录 link count 和 `..`
处理前破坏目录结构。

对于持久化镜像里的旧 0 字节缓存，可以直接让 Cargo 自动重建；若要立即消除 warning，可在 guest
中删除一次增量目录：

```bash
rm -rf /work/tgoskits/target/debug/incremental
```

这会失去该目录已有的增量结果，只是缓存清理，不是内核修复的一部分。

## 涉及文件

- `crates/lwext4_rust/c/lwext4/src/ext4.c`

## 验证

- host lwext4 C 构建通过。
- host rename 覆盖回归 `/tmp/lwext4_rename_replace_test` 输出
  `lwext4 rename replace: PASS`，覆盖旧目标、源路径消失、目标内容更新和同 inode no-op 均通过。
- `e2fsck -fn /tmp/lwext4-rename-replace.img` 五阶段通过。
- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- 最新 RISC-V 日志推进到 Cargo `444/446`，无原 `EEXIST` 发布失败；尚未取得完整 BuildStorm 结束标记。

