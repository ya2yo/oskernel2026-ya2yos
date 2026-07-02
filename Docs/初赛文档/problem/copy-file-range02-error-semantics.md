# copy_file_range02 错误路径语义修复

## 背景

LTP `copy_file_range02` 不是普通成功复制用例，而是集中验证 `copy_file_range()` 在错误路径上的 errno。该用例运行前会通过 `needs_device` / `format_device` / `mount_device` 准备一个 ext2 挂载点，因此要先保证测试环境能在 `$PATH` 中找到 `mkfs.ext2`，并且 `/dev/loop*` 至少具备格式化工具需要的块设备兼容行为。

本次分析以 LoongArch 单跑 musl/glibc `copy_file_range02` 的 `log.ans` 为依据。

## 现象

最初日志显示测试没有进入 syscall 断言阶段，而是在准备阶段被跳过：

```text
TCONF: Couldn't find 'mkfs.ext2' in $PATH
```

补齐 `mkfs.ext2` applet 入口后，测试能找到格式化工具，但 `mkfs.ext2` 在探测 loop 设备时失败：

```text
mkfs.ext2: lseek(0, 2): Invalid seek
TBROK: mkfs.ext2 failed with exit code 1
```

补齐 loop 块设备基本行为后，测试进入 `copy_file_range()` 断言阶段，暴露出 readonly、目录、`O_APPEND`、无效 flags、重叠区间、特殊文件、超大 length 和目标范围溢出等错误路径 errno 与 Linux 语义不一致。

最终 `log.ans` 中 musl/glibc 两轮测试均输出：

```text
Summary:
passed   24
failed   0
broken   0
skipped  14
warnings 0
```

其中 `FAIL LTP CASE copy_file_range02 : 10` 和 `RESULT GLIBC LTP SINGLE CASE copy_file_range02 : 10` 是当前测试包装层打印的退出码行，不能替代 LTP Summary 判断。该用例应以 `TPASS` / `TFAIL` / `TBROK` 和 Summary 为准。

## 分析

测试正常推进需要满足三层条件：

1. 用户态环境中存在 `/bin/mkfs.ext2` 或 `/bin/mke2fs`，否则 LTP 在 `format_device` 阶段直接 `TCONF`。
2. `/dev/loop*` 对 `mkfs.ext2` 呈现为可 seek、可查询大小和扇区大小的块设备，否则格式化阶段 `TBROK`。
3. `sys_copy_file_range()` 在进入复制前按 Linux 语义完成错误条件校验，否则测试会进入断言但报告 `TFAIL`。

LTP `copy_file_range02` 的关键期望包括：

| 场景 | 期望 errno |
|------|------------|
| 输入 fd 只写或已关闭 | `EBADF` |
| 输出目标是目录 | `EISDIR` |
| 输出 fd 带 `O_APPEND` | `EBADF` |
| `flags != 0` | `EINVAL` |
| 同一文件范围重叠 | `EINVAL` |
| block / char / fifo / pipe 等特殊 fd | `EINVAL` |
| `count > SSIZE_MAX` | `EOVERFLOW` |
| 目标范围超过最大文件大小 | `EFBIG` |

原实现的问题是 `sys_copy_file_range()` 主要按成功复制路径组织，缺少这些前置校验；测试环境侧也缺少 `mkfs.ext2` applet 入口和 loop 块设备的 `lseek(SEEK_END)`、容量、扇区查询兼容。

## 根因

`copy_file_range02` 看似是单个 syscall 测例，但实际先依赖 ext2 loop mount 环境，再验证 syscall 的错误语义。Ya2yOS 同时缺少三项能力：

- 初始化文件中没有创建 `/bin/mkfs.ext2` / `/bin/mke2fs` 到 busybox 的 applet 链接，导致 LTP 准备阶段找不到格式化工具。
- `DevLoop` 只接受部分 ioctl，缺少 per-open offset、`lseek()`、默认容量和扇区大小查询，导致 `mkfs.ext2` 无法把 `/dev/loop*` 当成块设备使用。
- `sys_copy_file_range()` 没有按 Linux 语义区分 fd 权限、文件类型、`O_APPEND`、flags、重叠区间和极限长度错误码。

## 修复

| 文件 | 修改 |
|------|------|
| `os/src/fs/kernel_fs_ops/initfiles.rs` | 在 busybox applet symlink 列表中加入 `/bin/mkfs.ext2` 和 `/bin/mke2fs`，使 LTP `format_device` 能找到 ext2 格式化工具 |
| `os/src/fs/files/loopdev.rs` | 为 `DevLoop` 增加 per-open offset、默认 64MiB 容量、512 字节扇区、零填充 `read()`、推进 offset 的 `write()`、`lseek(SEEK_SET/CUR/END)`、`fstat()` 大小信息，以及 `BLKGETSIZE64` / `BLKGETSIZE` / `BLKSSZGET` ioctl |
| `os/src/syscall/fs/io.rs` | 在 `sys_copy_file_range()` 中补齐 `flags`、`count`、fd 权限、文件类型、目录输出、`O_APPEND`、负 offset、目标范围溢出、同文件重叠范围等前置校验，并返回 LTP 期望的 Linux errno |

这次没有修改全局 `OpenFlags` 读写权限判定，因为已有初始化路径依赖 `O_CREATE` 单独打开后写入。`copy_file_range()` 只在 syscall 局部按 fd access mode 和 file object 能力做判定，避免扩大行为影响面。

## 验证

已执行：

```text
make TARGET_ARCH=loongarch64
```

结果：构建通过。

维护者提供的最新 `log.ans` 显示：

- musl `copy_file_range02`：`passed 24 failed 0 broken 0 skipped 14 warnings 0`
- glibc `copy_file_range02`：`passed 24 failed 0 broken 0 skipped 14 warnings 0`
- readonly、directory、append、closed fd、invalid flags、overlap、block/char/fifo/pipe、max length、max file size 等断言均输出 `TPASS`

日志中的 `chattr` / `mkswap` / `swapon` / `swapoff` 缺失导致部分子项 `TCONF` / skipped，属于当前环境不支持 immutable/swapfile 场景，不影响该用例已覆盖错误路径的通过结论。

未运行 `riscv64`。
