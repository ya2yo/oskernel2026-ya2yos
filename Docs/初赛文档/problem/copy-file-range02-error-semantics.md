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

随后补齐 `/bin/chattr`、`/bin/mkswap`、`/bin/swapon`、`/bin/swapoff` applet 后，immutable 子项不再被跳过，又暴露出两个新失败：

```text
copy_file_range02.c:149: TFAIL: copy_file_range returned wrong value: 32
copy_file_range02.c:149: TFAIL: copy_file_range returned wrong value: 16
```

对应场景分别是 immutable file 和 overlapping range。第一次修复 immutable 时曾在 `copy_file_range()` 中对通用 `dyn File` 调用 `path()`，在 block/char/fifo/pipe 等特殊 fd 场景会落到 `File::path()` 默认实现，触发：

```text
[kernel] Panicked at src/fs/vfs.rs:147 not implemented: File::path
```

最终 `log.ans` 中 musl/glibc 两轮测试均输出：

```text
Summary:
passed   26
failed   0
broken   0
skipped  6
warnings 0
```

此前环境未启用 `chattr` / swap applet 时，Summary 为：

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
| immutable 输出文件 | `EPERM` |
| 同一文件范围重叠 | `EINVAL` |
| block / char / fifo / pipe 等特殊 fd | `EINVAL` |
| `count > SSIZE_MAX` | `EOVERFLOW` |
| 目标范围超过最大文件大小 | `EFBIG` |

原实现的问题是 `sys_copy_file_range()` 主要按成功复制路径组织，缺少这些前置校验；测试环境侧也缺少 `mkfs.ext2` applet 入口和 loop 块设备的 `lseek(SEEK_END)`、容量、扇区查询兼容。后续启用 `chattr` 后，busybox 通过 `FS_IOC_GETFLAGS` / `FS_IOC_SETFLAGS` 设置 `FS_IMMUTABLE_FL`，而 `OSFile` 原先没有文件属性 ioctl 和 immutable 写保护，所以 immutable 子项会错误成功复制。overlap 子项失败则是因为同文件判断只比较路径字符串，在不同 fd/path 缓存场景下不可靠，应按 `stat` 的 `st_dev/st_ino` 判断同一 inode。

## 根因

`copy_file_range02` 看似是单个 syscall 测例，但实际先依赖 ext2 loop mount 环境，再验证 syscall 的错误语义。Ya2yOS 同时缺少三项能力：

- 初始化文件中没有创建 `/bin/mkfs.ext2` / `/bin/mke2fs` 到 busybox 的 applet 链接，导致 LTP 准备阶段找不到格式化工具。
- `DevLoop` 只接受部分 ioctl，缺少 per-open offset、`lseek()`、默认容量和扇区大小查询，导致 `mkfs.ext2` 无法把 `/dev/loop*` 当成块设备使用。
- `sys_copy_file_range()` 没有按 Linux 语义区分 fd 权限、文件类型、`O_APPEND`、flags、immutable 输出、重叠区间和极限长度错误码。
- `OSFile` 没有实现 ext2 flags ioctl，也没有在写入路径检查 `FS_IMMUTABLE_FL`。
- overlap 判断依赖路径字符串，不能稳定识别同一个 inode。

## 修复

| 文件 | 修改 |
|------|------|
| `os/src/fs/kernel_fs_ops/initfiles.rs` | 在 busybox applet symlink 列表中加入 `/bin/mkfs.ext2`、`/bin/mke2fs`、`/bin/chattr`、`/bin/mkswap`、`/bin/swapon`、`/bin/swapoff`，使 LTP `format_device` 和 immutable/swapfile setup 能找到对应 busybox applet |
| `os/src/fs/files/loopdev.rs` | 为 `DevLoop` 增加 per-open offset、默认 64MiB 容量、512 字节扇区、零填充 `read()`、推进 offset 的 `write()`、`lseek(SEEK_SET/CUR/END)`、`fstat()` 大小信息，以及 `BLKGETSIZE64` / `BLKGETSIZE` / `BLKSSZGET` ioctl |
| `os/src/fs/files/os_file.rs` | 增加路径级 ext2 flags 映射，支持 `FS_IOC_GETFLAGS` / `FS_IOC_SETFLAGS` / 32 位变体，并在普通文件 `write()` 前对 `FS_IMMUTABLE_FL` 返回 `EPERM` |
| `os/src/syscall/fs/io.rs` | 在 `sys_copy_file_range()` 中补齐 `flags`、`count`、fd 权限、文件类型、目录输出、`O_APPEND`、immutable 输出、负 offset、目标范围溢出、同文件重叠范围等前置校验；同文件判断改为 `st_dev/st_ino`，避免 path 比较漏判 |

这次没有修改全局 `OpenFlags` 读写权限判定，因为已有初始化路径依赖 `O_CREATE` 单独打开后写入。`copy_file_range()` 只在 syscall 局部按 fd access mode 和 file object 能力做判定，避免扩大行为影响面。

## 验证

已执行：

```text
make TARGET_ARCH=loongarch64
make TARGET_ARCH=riscv64
timeout 120s make run > log.ans 2>&1
```

结果：

- LoongArch 构建通过。
- RISC-V 构建通过。
- LoongArch `make run` 正常 `shutdown!`。

最新 `log.ans` 显示：

- musl `copy_file_range02`：`passed 26 failed 0 broken 0 skipped 6 warnings 0`
- glibc `copy_file_range02`：`passed 26 failed 0 broken 0 skipped 6 warnings 0`
- immutable file 输出 `TPASS: copy_file_range failed as expected: EPERM (1)`
- overlapping range 输出 `TPASS: copy_file_range failed as expected: EINVAL (22)`
- readonly、directory、append、closed fd、invalid flags、block/char/fifo/pipe、max length、max file size 等断言均输出 `TPASS`

日志中 `swapon: file_swap: file has holes` 和 `swapoff: Function not implemented` 导致 swapfile 子项 `TCONF` / skipped，当前不影响 `copy_file_range02` 已执行错误路径的通过结论。
