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

## 进一步优化

初版修复后 `open14` 已能通过，但深层目录下仍有大量 `open()` 会在完整路径 cache miss 后从根 inode 重新查找父目录；`create_file()` 也会为了权限检查、`S_ISGID` gid 继承和最终创建重复解析父路径。

本次继续收窄 `open` 热路径：

- 新增 `resolve_parent_path()`，一次性解析并返回父目录 inode 与规范化 create path。
- `create_file()` 复用同一个父目录 inode 完成写/执行权限检查、`S_ISGID` gid 继承和 `parent_inode.create()`，避免每个阶段重新查找父目录。
- `open_inner()` 在完整路径 cache miss 时先尝试 `find_from_cached_parent()`：如果父路径已在 `FsIndex` 中，直接用缓存父 inode 拼接真实父路径查找 child，并把 child 的规范化路径和调用路径都写回 inode cache。
- `Ext4Inode::types()` 改为读取 inode 对象构造时已有的类型，不再为了 `is_dir()` / `O_DIRECTORY` 判断调用 `live_path()` 和底层 `file_type()` 路径查询。

需要注意的是，当前 `lwext4_rust` wrapper 暴露的是 `check_inode_exist(path, type)` / `file_open(path, flags)` 这类路径接口，没有可直接从父目录句柄查相对子项的安全封装。因此这次优化复用了 VFS 层已缓存父 inode，减少父目录重复解析和元数据路径查询；底层 ext4 对 child 的存在性判断仍然是路径式 API。

## Dentry cache 优化

在父目录 inode 已经缓存后，继续从底层 ext4 路径接口查 child 仍会产生重复目录项查询。为进一步利用已缓存父目录，本次在 `os/src/fs/dcache.rs` 实现 VFS dentry cache：

- cache key 使用“父 inode 对象地址 + child name”，命中时不需要额外 `fstat()` 或 `path()` 计算 key，避免把 cache hit 变成新的元数据查询。
- positive dentry 保存 `Weak<dyn Inode>`，不会因为 dentry cache 自身额外延长 inode 生命周期；weak 失效后自动删除该项并回退到底层查找。
- negative dentry 只在非 `O_CREAT` 路径上使用，避免 create-if-not-exist 场景被旧的不存在缓存误导。
- `find_from_cached_parent()` 先查 dentry cache：positive 命中直接返回 child inode，negative 命中直接返回 `ENOENT`，miss 才调用底层 `parent_inode.find()`。
- `create_file()` 成功后回填 positive dentry；直接 `mknod`、`linkat`、`O_TMPFILE` materialize 也在插入 `FsIndex` 后回填。
- `unlinkat`、`symlinkat`、`renameat2` 成功后失效对应 dentry，避免 stale positive/negative 影响后续 `open()` 语义。

该实现仍然保留前一节提到的限制：底层 lwext4 没有暴露真正的“父目录句柄 + child name”相对查找接口，所以 dentry miss 时仍会走路径式 API；但 dentry hit 可以直接绕过这一步。

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

进一步优化后再次执行：

```text
make
timeout 600s make run
```

结果仍为：

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

本轮只验证默认 RISC-V；未额外执行 `TARGET_ARCH=loongarch64`。

Dentry cache 接入后再次执行：

```text
make
timeout 600s make run
```

结果仍为：

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

本轮只验证默认 RISC-V；未额外执行 `TARGET_ARCH=loongarch64`。
