# LTP open14: O_TMPFILE 匿名临时文件语义

## 背景

LTP `open14` 验证 `open(..., O_TMPFILE | O_RDWR, mode)` 的 Linux 语义。这个接口名字里有 `TMPFILE`，但它不是在目录下创建一个叫 `.tmp` 或 `0.tmp` 的普通文件，而是在指定目录所属文件系统里创建一个未链接到任何目录项的普通文件，并直接返回这个文件的 fd。

这里最容易误解的是 `open()` 参数和返回对象不是同一种东西：

- `openat(dirfd, path, O_TMPFILE | O_RDWR, mode)` 中的 `path` 必须指向一个目录，用来决定匿名 inode 放在哪个文件系统、继承哪个目录环境。
- syscall 返回的 fd 指向一个普通文件，不是目录 fd。
- 文件没有名字，没有目录项，`readdir()` / `getdents64()` 不应看到它。
- fd 关闭前数据仍然存在于这个打开文件对象里，可以 `read()`、`write()`、`lseek()`、`fstat()`。
- 只有后续通过 `linkat("/proc/self/fd/<fd>", "name", AT_SYMLINK_FOLLOW)` 这类路径把它链接到目录树后，它才会变成一个可见文件。

因此 `O_TMPFILE` 的核心语义是“匿名 inode + 打开文件描述符”，不是“自动取名的临时路径”。

## 现象

最初 `log.ans` 中 `open14` 在创建 `O_TMPFILE` 后检查当前目录，发现了内核创建出来的真实临时文件：

```text
open14 0 TINFO: creating a file with O_TMPFILE flag
open14 0 TINFO: looking for the file in '.'
open14 1 TFAIL: found unexpected file: 0.tmp
```

修复过程中又陆续暴露出几个相关问题：

- 目录已经不再出现 `0.tmp` 后，`getdents64()` 第二次读取空目录返回 `EINVAL`，导致 `readdir()` 失败。
- `readlink("/proc/self/fd/<fd>")` 返回 `ENOENT`，测试无法拿到 tmpfile 的 fd 路径。
- `linkat("/proc/self/fd/<fd>", "tmpfile", AT_SYMLINK_FOLLOW)` 不能把匿名文件物化成普通文件。
- 早期修复中在 `O_TMPFILE` 分支里复用高层 `open(abs_path, O_DIRECTORY, ...)` 校验目录，触发当前进程锁重入，panic 于 `process.rs:155 fail to get proc lock(2)`。
- 后续日志中大量重复的 `Chdir ret = 0` / `Unlinkat ret = 0` 看起来像死循环，实际是 `open14` 第二个子项创建并删除 100 层嵌套目录，debug 输出过多导致运行时间变长。

最终通过的 `log.ans` 显示 musl 与 glibc 两轮 `open14` 均通过：

```text
open14 1 TPASS: single file tests passed
open14 2 TPASS: multiple files tests passed
open14 3 TPASS: file permission tests passed

Summary:
passed   3
failed   0
broken   0
```

## 分析

`open14` 的三个子项覆盖了 `O_TMPFILE` 的主要行为：

1. 单文件场景：创建匿名 tmpfile，写入 4 KiB，`fstat()` 检查大小；随后读取目录，确认目录里没有这个文件；再通过 `/proc/self/fd/<fd>` + `linkat()` 把它链接成 `tmpfile`，确认链接后目录里能看到它。
2. 多目录场景：创建 100 层嵌套目录，在每层目录里打开一个匿名 tmpfile；随后从最深层往外删除目录，但已经打开的 tmpfile fd 仍应可读写，说明 tmpfile 生命周期由 fd 持有，而不是由目录项持有。
3. 权限场景：用不同 mode 创建匿名 tmpfile，写入后 link 到目录树，再 `lstat()` 检查物化后的权限是否来自创建时传入的 mode 和当前 umask。

原实现为了快速支持 `O_TMPFILE`，在目标目录下生成 `N.tmp` 这种真实文件，然后返回这个普通文件 fd。这能让 `read/write/fstat` 工作，但破坏了 `O_TMPFILE` 最重要的匿名性：文件已经有了目录项，`getdents64()` 必然能枚举到它。

同时，Linux 用户态通常用 `/proc/self/fd/<fd>` 表达“这个 fd 当前指向的文件”。对于匿名 tmpfile，`readlink()` 常见显示形式带有 `(deleted)`，表示它没有普通目录项；但这条 procfd 路径仍可被 `linkat(..., AT_SYMLINK_FOLLOW)` 用来给匿名文件创建一个正式名字。内核之前没有这部分兼容路径，导致匿名 tmpfile 即使创建正确，也无法被 `open14` 后半段物化。

## 根因

根因是把 `O_TMPFILE` 误实现成“在目录里创建一个真实临时文件名”：

1. `O_TMPFILE` 的 `path` 应该是目录，目录只用于定位文件系统和权限上下文；原实现却把它拼接成 `${dir}/${counter}.tmp`。
2. 原实现创建了真实目录项，违反匿名 tmpfile 在 link 前不可被 `readdir()` 发现的语义。
3. 缺少独立的匿名文件对象，导致 tmpfile 生命周期无法只由 fd 持有。
4. 缺少 `/proc/self/fd/<fd>` 的 `readlinkat()` 和 `linkat()` 兼容，匿名文件无法按测试预期链接到目录树。
5. `getdents64()` EOF 位置使用 `usize::MAX` 表示后，又通过普通 `lseek(SEEK_CUR)` 读取 offset，第二次读空目录时把 EOF cookie 当成普通文件 offset，错误返回 `EINVAL`。
6. `O_TMPFILE` 分支位于 `sys_openat()` 已持有进程内部锁的路径中，复用会重新取当前进程锁的高层 `open()` 做目录校验，引入锁重入 panic。

## 修复

新增 `TmpFile` 文件类型，用内存对象表达未链接的普通文件：

- 持有 `Vec<u8>` 数据和当前 offset，支持 `read()`、`write()`、`lseek()`。
- `fstat()` 返回 `S_IFREG | mode`，`st_size` 来自内存数据长度，`st_nlink = 0` 表示当前未链接到目录树。
- 记录创建时的 effective uid/gid 与经过 umask 处理后的 mode，确保后续物化时能保留权限语义。
- 分配假的 inode 号，仅用于 `fstat()` 兼容；它不进入 `FsIndex`，也不创建 ext4 目录项。

修改 `sys_openat()` 的 `O_TMPFILE` 分支：

- 拒绝只读 `O_TMPFILE`，保持 Linux 兼容的 `EINVAL`。
- 将传入路径作为目录校验，要求目标 inode 是目录。
- 不再拼接 `0.tmp` / `N.tmp`，也不再调用普通 `open()` 创建真实文件。
- 直接创建 `FileClass::Abs(TmpFile)` 并放入 fd table。
- 在 `FSInfo` 中登记合成 procfd 目标，如 `"/tmp/LTP_xxx/#tmpfile-3 (deleted)"`，供 `readlink("/proc/self/fd/3")` 使用。
- 目录校验改用 `FsIndex` / `superblock_root_inode().find(..., O_DIRECTORY, ...)`，避免在 syscall 持锁区间重入高层 `open()`。

补齐 procfd 相关行为：

- `FSInfo::fd_path(fd)` 返回 fd 对应的路径字符串。
- `sys_readlinkat()` 识别 `/proc/self/fd/<fd>`，返回 `FSInfo` 中记录的合成路径，不追加 NUL。
- `sys_linkat()` 识别旧路径为 `/proc/self/fd/<fd>` 时，从源 fd 读取内容，创建目标普通文件并写入数据，然后把目标 inode 插入 `FsIndex`。该路径用于把匿名 tmpfile 物化成目录树中的普通文件。

修复 `getdents64()` EOF 行为：

- 为 `OSFile` 增加直接读取当前 offset 的接口。
- `sys_getdents64()` 不再用普通 `lseek(0, SEEK_CUR)` 读取目录流位置。
- 当目录 offset 已是 `usize::MAX` 时直接返回 `0`，表示 EOF，避免第二次 `readdir()` 把空目录 EOF 错误转换成 `EINVAL`。

涉及文件：

- `os/src/fs/files/tmp_file.rs`
- `os/src/fs/files/mod.rs`
- `os/src/fs/files/os_file.rs`
- `os/src/fs/fs_info.rs`
- `os/src/syscall/fs/fd_ops.rs`
- `os/src/syscall/fs/ctl.rs`

## 验证

已执行：

```text
make
```

结果：

- 当前默认架构为 `loongarch64`，构建通过。
- 维护者提供的最新 `log.ans` 显示 musl `open14` 三个子项均为 `TPASS`，Summary 为 `passed 3 failed 0 broken 0`。
- 维护者提供的最新 `log.ans` 显示 glibc `open14` 三个子项均为 `TPASS`，Summary 为 `passed 3 failed 0 broken 0`。
- 日志中的 `FAIL LTP CASE open14 : 0` 是当前包装层打印格式，真实 LTP Summary 中 failed 为 0。
- AI 环境中的 `make run` 曾受 `/var/tmp` 沙箱限制不能稳定启动 QEMU；最终运行结果以维护者本地 `log.ans` 为准。
- 未运行 `riscv64`。
