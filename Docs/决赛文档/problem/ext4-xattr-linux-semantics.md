# EXT4 xattr syscall 与 Linux 语义对齐

## 背景

竞赛镜像的 LTP 文件系统组包含 `setxattr`、`getxattr`、`listxattr`、`removexattr` 及其
`l*`/`f*` 变体。lwext4 已提供 pathname xattr C API，但 Ya2yOS 在本轮前没有将其接入
VFS：多数 syscall 只记录日志后返回成功，`getxattr()` 则固定返回 `ENODATA`。

## 现象

- 12 个 xattr syscall 入口原先没有数据路径，长度查询和用户缓冲区也未实现。
- syscall 分发把 `lremovexattr` 错误调用成普通 pathname 版本，并把 `fremovexattr` 当作 pathname 调用。
- C 侧 `ext4_removexattr()` 在 pathname 打开失败时再次请求 mount lock，遗留已持有的锁。

## Linux 对照

Linux 7.0 的 `fs/xattr.c` 以 `path_*xattrat()` 统一 pathname、符号链接 follow/no-follow 与 fd
入口：普通入口使用 `LOOKUP_FOLLOW`，`l*` 使用 `AT_SYMLINK_NOFOLLOW`，`f*` 使用 `AT_EMPTY_PATH`。
`setxattr_copy()` 检查未知 flag 位、属性名和 `XATTR_SIZE_MAX`；读取和列举把过大的用户 buffer
截断至 64 KiB，只有底层仍报告 `ERANGE` 才转换为 `E2BIG`。

Linux EXT4 的 `ext4_xattr_set_handle()` 在 xattr 写锁内查找属性并按 `XATTR_CREATE`/
`XATTR_REPLACE` 判定。两个 flag 同时传入并不在 syscall 层拒绝：属性已存在时因 `CREATE`
返回 `EEXIST`，不存在时因 `REPLACE` 返回 `ENODATA`。

详见 `/home/ya2yo/learning_linux/ext4-xattr-vfs-syscall-analysis.md`。

## 根因

Ya2yOS 的 `Inode` trait 没有 xattr 能力边界，EXT4 adapter 也没有 Rust wrapper。因此 syscall
层无法把经用户内存校验的请求委托给后端。lwext4 的 `ext4_setxattr()` 本身没有 flags 参数，
若把“先 get 后 set”拆成两个临界区，会在多 hart 下产生 `CREATE/REPLACE` 的 TOCTOU。

## 修复

- 在 `Inode` 添加 `set_xattr`、`get_xattr`、`list_xattr`、`remove_xattr`；不支持的后端统一返回
  `EOPNOTSUPP`。
- 在 `Ext4Inode` 接入 lwext4 xattr API。存在性查验与实际 set 在同一次
  `write_state -> io_state -> EXT4_OP_LOCK` 临界区内完成。
- 将 lwext4 C API 封装为 Rust 方法，支持 NULL/size=0 长度查询和 NUL 分隔的属性列表。
- 完整实现 12 个 Linux xattr syscall 入口。属性名限制为 255 字节，值和列表限制为 65536
  字节；空名称为 `ERANGE`、过大 set value 为 `E2BIG`、不正确指针为 `EFAULT`。
- 普通 pathname 请求跟随末级符号链接；`l*` 通过项目内部 `O_UNLINK` 取得末级链接 inode；
  `f*` 对 `O_PATH` 描述符返回 `EBADF`。
- 修正 remove syscall 分发，并修复 lwext4 `ext4_removexattr()` 错误分支解锁。

## 涉及文件

- `os/src/syscall/fs/xattr.rs`
- `os/src/syscall/mod.rs`
- `os/src/fs/vfs.rs`
- `os/src/fs/ext4_lw/inode/metadata.rs`
- `os/src/fs/ext4_lw/inode/vfs.rs`
- `crates/lwext4_rust/src/file.rs`
- `crates/lwext4_rust/c/lwext4/src/ext4.c`

## 验证

- `cargo fmt --manifest-path os/Cargo.toml --all -- --check` 通过。
- `git diff --check` 通过。
- RISC-V 与 LoongArch64 release `make build-arch` 通过。
- 曾构建临时 RISC-V guest 并尝试调用 18 个 xattr LTP 用例；guest 正常启动和 `shutdown!`，但当前
  final-2026 镜像的 `/glibc/ltp/testcases/bin` 不包含对应二进制，全部在 `execve` 前以 127 退出。
  该结果不能作为 xattr 内核语义通过或失败的证据，临时 `initproc` 路由已恢复。
- 没有运行完整 BuildStorm 或完整 LTP：维护者已明确当前 EXT4 尚未完成，二者无法在一小时内完成。

## 未覆盖边界

- 尚未实现 Linux VFS 的 inode mode/owner/sticky-dir xattr 权限模型、`trusted.*` capability、
  immutable/append-only、mount idmap、LSM、fsnotify 和 POSIX ACL 专用路径。
- 当前路径字符串仍遵从仓库级 `MAX_PATH_LEN = 256`，小于 Linux `PATH_MAX`。
- 需要装载含 LTP xattr 可执行文件的镜像后，再运行基础 set/get/list/remove、fd、长度查询和 symlink
  用例作为动态回归。
