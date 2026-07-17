# linkat01 dirfd、procfs 跨设备与 flags 语义修复

## 背景

LTP `linkat01` 覆盖 `linkat(2)` 的普通链接、绝对/相对路径、`dirfd`、跨设备、目录和 flags 错误码语义。维护者将 initproc 入口切换为该测例的 musl/glibc 最小回归。

## 现象

新的 `log.ans` 中，musl 与 glibc 均为 `passed 18 failed 4 broken 0`。四个失败项一致：

- 相对源路径以 stdin 作为 `olddirfd`，预期 `ENOTDIR`，实际为 `EINVAL`；
- 相对目标路径以 stdin 作为 `newdirfd`，预期 `ENOTDIR`，实际为 `EINVAL`；
- 将 `/proc/cpuinfo` 链接到测试目录，预期 `EXDEV`，实际成功；
- 传入 flags `1`，预期 `EINVAL`，实际成功。

## 分析

`Process::get_abs_path()` 是通用路径归一化 helper：对非 `AT_FDCWD` 的相对路径，它尝试从 fd 的关联文件路径拼接路径，但不负责检查该 fd 是否为目录。stdin 不是 `OSFile`，`FileDescriptor::file()` 的类型转换因此返回 `EINVAL`，泄露给 `linkat`，而 Linux 要求在这种场景返回 `ENOTDIR`。

项目的 `/proc/cpuinfo` 是为 LTP 兼容创建在 root ext4 后端中的静态 proc 文件。`check_link_mounts()` 之前只查询可变挂载表，没有将该 proc 兼容命名空间识别为独立的 Linux-visible pseudo-filesystem，于是把 `/proc/cpuinfo` 与普通测试目录错误视为同一文件系统。`sys_linkat()` 也未校验 flags，导致未知 bit 直接被忽略。

## 根因

`sys_linkat()` 缺少 syscall 专属的相对 `dirfd` 目录校验、proc compatibility namespace 的跨设备识别和 flags 白名单。将这三类语义依赖于底层 ext4 或通用路径 helper，会产生与 Linux 不一致的 errno 或错误成功。

## 修复

在 `os/src/syscall/fs/ctl.rs` 中：

- 新增 `LINKAT_VALID_FLAGS`，仅接受 `AT_SYMLINK_FOLLOW | AT_EMPTY_PATH`，未知位返回 `EINVAL`；
- 新增 `resolve_linkat_path()`。绝对路径继续忽略 `dirfd`；相对路径的非 `AT_FDCWD` fd 必须是目录 `OSFile`，无效 fd 返回 `EBADF`，其它非目录对象返回 `ENOTDIR`；
- 正常路径和 `AT_EMPTY_PATH` 的目标路径均使用该 resolver；
- `check_link_mounts()` 在普通挂载表比较之前识别 `/proc` pseudo-filesystem 边界，普通路径与 `/proc` 间的 hard link 返回 `EXDEV`。

修复不改变通用 `Process::get_abs_path()`，避免将 `linkat` 的目录约束误施加到其它 syscall 或内部调用。

## 涉及文件

- `os/src/syscall/fs/ctl.rs`

## 验证

```text
RISC-V:    musl/glibc linkat01 均为 passed 22 failed 0 broken 0
LoongArch: musl/glibc linkat01 均为 passed 22 failed 0 broken 0
```

双架构 release 构建通过，QEMU 回归均正常 `shutdown!`；构建输出只有既有 vendored `smoltcp` warnings。
