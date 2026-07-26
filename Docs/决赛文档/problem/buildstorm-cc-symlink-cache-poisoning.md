# BuildStorm `cc` 符号链接缓存污染

## 背景

final-2026 的 Debian 根文件系统中，`/usr/bin/cc` 经由
`/etc/alternatives/cc`、`/usr/bin/gcc` 等符号链接指向实际 GCC 二进制。Ya2yOS
的 lwext4 文件打开接口不自动跟随符号链接，因此 VFS 必须在普通打开前完成解析，
同时不能让查看链接本身的操作改变普通打开的缓存结果。

## 现象

此前 BuildStorm 日志在 Cargo 并行预构建刚开始时首先出现：

```text
[ERROR] ext4_fopen: /usr/bin/cc, rc = 2
cc: fatal error: '-fuse-linker-plugin', but liblto_plugin.so not found
error: linking with `cc` failed: exit status: 1
```

`cc -print-file-name=liblto_plugin.so` 和独立 `cc -fuse-linker-plugin` 探针均
成功，说明插件文件和 GCC 搜索路径本身可用；后续 LTO 诊断只是编译器可执行文件未能
正确读入后的次生错误。

## 分析

`readlinkat`、`lstat` 与 unlink 等调用使用内部 `O_UNLINK`，要求 VFS 返回末级
symlink inode 自身。`open_inner()` 已在这类请求的缓存读取阶段绕过 `FsIndex`，
`find_from_cached_parent()` 也不读取 dentry cache，但两处成功查找后仍无条件调用
`FsIndex::insert_inode_idx()`。

因此，查看 `/usr/bin/cc` 链接的操作会把 `Ext4Inode::new("/usr/bin/cc",
SYMLINK)` 写入普通路径缓存。之后 Cargo 的普通 `open` 命中该缓存，不再解析链接；
`Ext4Inode::read_at()` 将 `/usr/bin/cc` 交给 `ext4_fopen()`。lwext4 只接受普通文件
目标，不会跟随该末级链接，返回 `ENOENT` (`rc = 2`)。

同时，Debian 的 `/lib -> /usr/lib` 等中间链接不能被当成末级链接处理。路径查找需先
识别当前分量是否为 symlink，再在普通文件/目录查找失败时展开中间链接。

## 根因

保留末级 symlink 的操作只隔离了缓存读取，没有隔离缓存写入，导致 symlink inode
污染普通路径缓存。这个缓存语义缺口使并发 Cargo 首次通过 `/usr/bin/cc` 启动链接器时
偶发或稳定地读到未解析的链接路径。

## 修复

- `os/src/fs/kernel_fs_ops/open.rs`：`O_NOFOLLOW` 与内部 `O_UNLINK` 的查找结果不再写入
  `FsIndex` 或 dentry positive cache；包括直接查找和通过已缓存父目录/解析父目录的重试
  分支。普通打开仍正常使用缓存。
- `os/src/fs/ext4_lw/inode.rs`：先识别末级 symlink，保持 `O_NOFOLLOW`、`O_UNLINK` 的
  Linux 可见语义；普通查找失败时递归展开首个中间 symlink，覆盖 Debian merged-/usr
  布局。

## 验证

- `cargo fmt --manifest-path os/Cargo.toml -- --check` 通过。
- `git diff --check` 通过。
- `make build-arch TARGET_ARCH=riscv64` 通过，仅有既有 `smoltcp` warning。
- 最新 RISC-V `log.ans` 通过 LTO 路径和链接探针，并进入 Cargo `37/446`；未再出现
  `ext4_fopen: /usr/bin/cc`、`liblto_plugin.so not found`、Cargo 链接错误、panic、
  `TFAIL` 或 `TBROK`。
- 日志在 `37/446` 后停止写入，未出现 `shutdown!` 或 BuildStorm 完成标记；完整 446 crate
  回归及 LoongArch64 回归尚未完成，不能据此宣称完整 BuildStorm 通过。
