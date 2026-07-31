# BuildStorm 新 inode 创建事务元数据合并

## 背景

P16 已将 `open(O_CREAT)` 的 mode/owner 处理合并到一个 VFS 创建入口，但 lwext4 仍在目录项创建完成后按路径重新查找 inode，再执行独立的 metadata transaction。BuildStorm 并发创建大量临时文件时，这条路径会重复占用 mount-wide EXT4 gate。

## 现象

`tmp_09.ans`、`tmp_10.ans` 的十分钟快照中，`metadata_apply` 分别约为 `35.1s`、`35.8s`，`mode` 分别约为 `40.2s`、`40.8s`。样本均只到 `24--25/446`，没有完整 BuildStorm 结束标记。

## 根因

新 inode 的初始 mode/uid/gid 属于创建事务的一部分，却经过 `fmode_set()`/`owner_set()` 的路径 API 再次查找和提交。创建时 VFS 已经知道最终类型、umask 和 owner，额外的 `file_type()` 查询也没有提供新的语义信息。

## 修复

- lwext4 `ext4_generic_open2()` 增加带初始 metadata 的内部入口，在 inode 分配和目录项链接前写入 mode、uid、gid；普通文件和目录分别提供带 metadata 的 `O_EXCL` 创建 API。
- Rust wrapper 新增 `file_open_with_metadata()` 和 `dir_mk_exclusive_with_metadata()`，旧 API 和后续通用 chmod/chown 路径保持不变。
- `Ext4Inode::create_with_metadata()` 按已知 `InodeType` 补齐 mode type bits，保留 umask、S_ISGID、非 root owner、父目录 epoch 和错误传播语义。

## 验证

- `tmp_11.ans` 已出现新实现的运行期证据：`metadata_apply(samples=0)`，`mode=783/3.534s`，`create=1182/27.259s`，无 `panic/TFAIL/TBROK/ERROR/SIGSEGV`；最后为 `Building 28/446`，未出现 `BUILDSTORM_COMPILE`、测试组 END 或 `shutdown!`，因此仅作方向性验证。
- `cargo fmt --manifest-path os/Cargo.toml --all -- --check`、`git diff --check` 通过。
- `make TARGET_ARCH=riscv64` 通过并继续完成 LoongArch64 release 子目标；两架构内核均编译成功。
- 仍待新建文件/目录、既有目标 EEXIST、非 root、S_ISGID、ENOSPC 回滚及完整 BuildStorm 结束样本。

## 2026-07-31：创建结果复用

`tmp_12.ans` 装载了创建最终组件后直接复用 `child_ref` 初始化 handle 的实现。末尾为
`t=568933ms`、Cargo `37/446`；create `1309/23.176s`，平均约 `17.71ms`，而 `tmp_11` 为
`1182/27.259s`，平均约 `23.06ms`。namespace gate 平均持有从约 `30.41ms` 降至 `23.65ms`。
日志无异常但没有完整结束标记；Cargo 进度差值受并发与宿主调度影响，不作为稳定加速率。

该样本仍有 `3183` 次真实 fstat，其中 `2799` 次归因为 cold inode。代码审计确认新建 inode 在
`FsIndex::cache_key()` 中因没有创建期 `(dev, ino)` 会立即执行一次 pathname fstat。P18.2.2 因此让
lwext4 在创建 transaction 内从现有 `child_ref` 返回完整 stat，并以 `new_with_stat()` 构造 VFS inode；
字段取值复用 `ext4_stat_get()` 的同一 helper，不伪造 `st_blocks` 或时间戳，也不增加块访问。

本轮 `cargo fmt --manifest-path os/Cargo.toml --all -- --check`、`git diff --check` 和按现有 lwext4
编译参数执行的宿主 C `-fsyntax-only` 检查通过。Docker 生成目录为只读且按维护者要求保持不动，故
P18.2.2 尚未完成 Rust 链接、双架构构建、定向语义回归或运行期验证。
