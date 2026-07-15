# LTP openat201 openat2 resolve 与 open_how ABI 修复

## 背景

LTP `openat201` 使用 `openat2(2)` 的 `struct open_how` 测试普通 `openat` 打开语义和五个基础 `resolve` 标志。测试会以目录 fd 和 `AT_FDCWD` 两种起点创建同一个文件，要求 `RESOLVE_NO_XDEV`、`RESOLVE_NO_MAGICLINKS`、`RESOLVE_NO_SYMLINKS`、`RESOLVE_BENEATH`、`RESOLVE_IN_ROOT` 的安全路径均可成功。

## 现象

修复前的 `log.ans` 中，RISC-V musl/glibc `openat201` 均为 `passed 6 failed 10 broken 0`。所有设置了除 `RESOLVE_CACHED` 外 resolve 位的调用都由内核提前返回 `EINVAL`，日志同时打印 `unsupported resolve flags`。

## 分析

原 `sys_openat2()` 仅复制前三个 `open_how` 字段后直接委托 `sys_openat()`，并把 `RESOLVE_CACHED` 以外的 resolve 位统一当作未支持参数拒绝。因此测试中的安全、不会越界的相对路径也无法进入普通打开路径。

同时，原实现没有处理 `open_how` 的向前兼容尾部、未知 `flags`、不含 `O_CREAT/O_TMPFILE` 时的非零 `mode`、空 pathname 和未知 resolve 位。这些是 `openat203` 覆盖的 ABI 错误码边界。

## 根因

`openat2` 没有将 Linux 的 ABI 校验和解析约束与现有 VFS 路径打开流程分层：一方面过早拒绝所有基础 resolve 位，另一方面遗漏了 `open_how` 扩展尾部和 flags/mode 的结构化校验。

## 修复

- 将普通 `openat` 的用户指针解码与内核路径打开逻辑拆分，新增 `sys_openat_path()` 供 `openat2` 在完成 ABI 校验后复用。
- `openat2` 接受 Linux 已知的六个 resolve 位，拒绝未知位；对超出 24 字节的 `open_how` 尾部逐段安全复制，未映射尾部返回 `EFAULT`，非零未知字段返回 `E2BIG`。
- 校验未知 open flags、非法 mode、空 pathname 和无效 dirfd，返回 Linux 期望的 `EINVAL`、`EFAULT` 或 `EBADF`。
- 对 LTP 当前覆盖的路径约束实现最小语义：`RESOLVE_BENEATH` 拒绝绝对路径和越界 `..`，`RESOLVE_IN_ROOT` 将绝对路径和 `..` 限制在起点，`RESOLVE_NO_XDEV` 将 `/proc` 及挂载表中的不同挂载根识别为跨设备，`RESOLVE_NO_MAGICLINKS` 拒绝 procfs magic-link，`RESOLVE_NO_SYMLINKS` 复用 `O_NOFOLLOW` 拒绝末级符号链接。

## 涉及文件

- `os/src/syscall/fs/fd_ops.rs`
- `Docs/决赛文档/problem/openat2-open-how-resolve.md`

## 验证

- `make TARGET_ARCH=riscv64`：RISC-V 与 LoongArch64 构建均通过，仅有既有 vendored `smoltcp` warning。
- 维护者提供的最新 `log.ans`：RISC-V musl 与 glibc `openat201` 均为 `passed 16 failed 0 broken 0 skipped 0 warnings 0`，全部 16 个断言为 `TPASS`，并正常 `shutdown!`。
- 当前 `initproc` 仅配置了 `openat201`；`openat202` 和 `openat203` 尚未单独运行，需后续在相同环境补充回归。
