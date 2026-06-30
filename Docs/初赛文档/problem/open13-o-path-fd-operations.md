# LTP open13: O_PATH fd 数据操作错误成功

## 背景

LTP `open13` 验证 `open(..., O_PATH)` 的基础语义：`O_PATH` 只获得可用于部分 fd 级别操作的路径句柄，并不会真正打开文件数据流。因此 `read(2)`、`write(2)`、`fchmod(2)`、`fchown(2)` 和 `fgetxattr(2)` 在该 fd 上都应失败并返回 `EBADF`。

## 现象

新的 `log.ans` 中 `open13` 失败：

```text
open13 1 TFAIL: read(2) succeeded unexpectedly
open13 2 TFAIL: write(2) failed unexpectedly, expected EBADF: TEST_ERRNO=SUCCESS(0)
open13 3 TFAIL: fchmod(2) succeeded unexpectedly
open13 4 TFAIL: fchown(2) failed unexpectedly, expected EBADF: TEST_ERRNO=ENOENT(2)
open13 5 TFAIL: fgetxattr(2) succeeded unexpectedly
```

修复中间版本后，glibc 已通过，但 musl 仍在 `fchmod/fchown` 上返回 `ENOENT`。临时日志确认 musl 会把 fd 操作包装为 `/proc/self/fd/<fd>` 路径访问：

```text
fchmodat dirfd=-100 path='/proc/self/fd/3'
fchownat dirfd=-100 path='/proc/self/fd/3'
```

## 分析

内核的 `FileDescriptor` 已保存 `OpenFlags`，但 `read/write/fchmod/fchown/fgetxattr` 入口没有检查 `O_PATH`，而是继续把 fd 当成普通打开文件使用。

此外，LoongArch musl 的 `fchmod()` / `fchown()` 兼容层没有直接使用内核的 `Fchmod` / `Fchown` 路径，而是通过 `/proc/self/fd/<fd>` 调用 `fchmodat/fchownat`。当前内核没有完整 procfd 解析，路径打开 `/proc/self/fd/3` 会落到普通文件系统查找并返回 `ENOENT`，导致 errno 不符合 `open13` 预期。

## 根因

1. syscall 层缺少对 `O_PATH` fd 的操作限制，导致数据读写和 inode 修改接口错误成功。
2. `fchown` 入口曾复用 `sys_fchownat(fd, NULL, ..., flags=0)`，空路径但没有 `AT_EMPTY_PATH` 时会返回 `ENOENT`，不符合 fd-only syscall 语义。
3. musl 通过 `/proc/self/fd/<fd>` 包装 fd 操作时，内核没有将该路径映射回 fd 语义。

## 修复

- 在 `FileDescriptor` 增加 `is_path_only()`，集中判断 `OpenFlags::O_PATH`。
- `sys_read()`、`sys_write()`、`sys_fchmod()`、`sys_fchown()`、`sys_fgetxattr()` 在取得 fd 后先检查 `O_PATH`，命中时返回 `EBADF`。
- `sys_fchownat()` 的 `AT_EMPTY_PATH` 空路径分支同样拒绝 `O_PATH` fd。
- 为 `Fchown` syscall 增加独立入口 `sys_fchown()`，避免用 `fchownat` 空路径无 flag 分支表达 fd-only 操作。
- 在 `fchmodat/fchownat` 路径分支中识别 `/proc/self/fd/<fd>`，将其映射回 fd 表；若目标 fd 是 `O_PATH`，优先按 fd 语义返回 `EBADF`。该兼容只覆盖当前 libc 包装需要的 procfd 形式，没有实现完整 procfs。

涉及文件：

- `os/src/fs/fstruct.rs`
- `os/src/syscall/fs/io.rs`
- `os/src/syscall/fs/ctl.rs`
- `os/src/syscall/fs/xattr.rs`
- `os/src/syscall/mod.rs`

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
git diff --check
```

结果：

- 当前默认 `TARGET_ARCH=loongarch64`，构建通过。
- `log.ans` 中 musl `open13` 五个子项均为 `TPASS`，Summary 为 `passed 5 failed 0 broken 0`。
- `log.ans` 中 glibc `open13` 五个子项均为 `TPASS`，Summary 为 `passed 5 failed 0 broken 0`。
- `git diff --check` 无输出。
- 未运行 `riscv64`。
