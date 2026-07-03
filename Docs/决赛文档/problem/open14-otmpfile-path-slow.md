# LTP open14 O_TMPFILE 深层路径超时

## 背景

`open14` 覆盖 `O_TMPFILE` 的基础语义：匿名临时文件写读、通过 `/proc/self/fd/<fd>` link 到目录、在多层目录中持有匿名 fd 后删除目录，以及 link 后检查权限位。

测例会构造 100 层 `tst02_*` 和 100 层 `tst03_*` 目录。每一层都会执行 `mkdir/chdir/open(".", O_TMPFILE)`，`test03` 还会写读匿名文件、`linkat("/proc/self/fd/<fd>", "tmpfile_<fd>", AT_SYMLINK_FOLLOW)` 和 `lstat()`。

## 现象

当前内核单跑 `open14` 时，串口最后长期停在：

```text
open14      0  TINFO  :  creating a file with O_TMPFILE flag
```

超过 5 分钟没有新输出。加临时阶段日志后确认系统没有死锁，测例实际在深层目录循环中缓慢推进；`test02`/`test03` 自身在长循环中没有持续打印进度，所以外观看起来像卡死。

## 分析

`open14` 的慢点主要来自深层绝对路径上的重复 VFS/ext4 查找：

- `sys_mkdirat()` 先 `open(abs_path, O_RDWR)` 检查目标是否存在，再 `open(abs_path, O_CREATE|O_DIRECTORY)` 创建目录；对预期不存在的深层路径做了两次完整路径解析。
- `open_inner()` 没有处理 `O_CREAT|O_EXCL` 的已存在语义，因此调用方只能在 syscall 层额外预查。
- ext4 `Inode::create()` 在目标已存在时返回现有 inode，而不是返回 `EEXIST`，无法承载 `O_EXCL` 语义。
- `linkat("/proc/self/fd/<fd>", newpath, AT_SYMLINK_FOLLOW)` 的 `O_TMPFILE` 兼容路径同样先查新路径是否存在，再创建目标文件。
- `sys_unlinkat()` 对目录删除也进入普通文件延迟删除路径，执行不必要的 `link_cnt()` 与 fd 路径扫描；普通文件 unlink 也在确认是否有 fd 前先查 link count。
- `open(".", O_TMPFILE)` 中，cwd 已由前序成功 `chdir()` 验证，但仍按完整绝对路径再次解析目录 inode。

这些操作单次都能返回，但 open14 在 100 层目录中重复执行，深层路径成本叠加后造成长时间无输出。

## 根因

根因不是 `O_TMPFILE` 文件对象阻塞，而是 VFS 创建/删除路径缺少 Linux `O_CREAT|O_EXCL` 语义承载，导致 syscall 层用额外 `open()` 预查目标存在性；同时目录删除和 cwd tmpfile 打开没有区分可跳过的校验路径。深层目录测例把这些重复查找放大成接近超时的慢路径。

## 修复

- `open_inner()` 支持 `O_CREATE|O_EXCL`：
  - 目标存在时返回 `EEXIST`。
  - 独占创建路径直接进入 `create_file()`，避免先查预期不存在的目标。
- `Ext4Inode::create()` 对同类型目标已存在返回 `EEXIST`，不再把 create(existing) 当作成功。
- `sys_mkdirat()` 改为单次 `open(O_RDWR|O_CREATE|O_EXCL|O_DIRECTORY)`，由通用 open/create 路径处理存在性。
- `O_TMPFILE` 的 `/proc/self/fd/<fd>` materialize 路径改为单次 `open(O_CREATE|O_EXCL|O_RDWR)` 创建目标，去掉额外存在性预查。
- `sys_unlinkat()` 对空目录 `rmdir` 直接 unlink 并移除 FsIndex，不走普通文件延迟删除检查；普通文件先查 `has_fd`，只有存在打开 fd 时才查 `link_cnt()`。
- `sys_openat()` 中 `AT_FDCWD + "." + O_TMPFILE` 复用当前 cwd 已验证事实，跳过一次完整目录 inode 解析。

涉及文件：

- `os/src/fs/kernel_fs_ops/open.rs`
- `os/src/fs/ext4_lw/inode.rs`
- `os/src/syscall/fs/ctl.rs`
- `os/src/syscall/fs/fd_ops.rs`

## 验证

已执行：

```text
make
timeout 600s make run
```

结果：

```text
open14      1  TPASS  :  single file tests passed
open14      2  TPASS  :  multiple files tests passed
open14      3  TPASS  :  file permission tests passed

Summary:
passed   3
failed   0
broken   0
skipped  0
warnings 0
```

`make run` 中仍有包装器行：

```text
FAIL LTP CASE open14 : 0
```

该行中的退出码为 0，且 LTP summary 为 `failed 0 broken 0`，按本仓库判读规则以 `TPASS/TFAIL/TBROK/Summary` 为准。

本次验证基于当前默认 RISC-V 配置；未额外执行 `TARGET_ARCH=loongarch64`。
