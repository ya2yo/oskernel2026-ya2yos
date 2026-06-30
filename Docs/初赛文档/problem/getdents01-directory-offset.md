# LTP getdents01: 目录流 offset 更新错误

## 背景

LTP `getdents01` 验证目录读取结果是否完整、正确。用例在临时目录中准备 `dir`、`file`、`symlink`，并期望 `getdents/getdents64` 能枚举出 `.`, `..`, `dir`, `file`, `symlink`。其中 raw `SYS_getdents64` 和 libc `getdents64()` 都会进入内核 `sys_getdents64()`。

测试源码路径：

- `/home/ya2yo/projects/OSKernel2026-PlainOs/testsuits-for-oskernel/ltp-full-20240524/testcases/kernel/syscalls/getdents/getdents01.c`
- `/home/ya2yo/projects/OSKernel2026-PlainOs/testsuits-for-oskernel/ltp-full-20240524/testcases/kernel/syscalls/getdents/getdents.h`

## 现象

修复前 `log.ans` 中 `getdents01` 在 `getdents64` 路径失败，用户态报告 `getdents failed unexpectedly`，内核 syscall 返回 `EINVAL`。

定位过程中加入日志后，关键信息为：

```text
[sys_getdents64] fd is 3, buf addr is ..., len is 512
read_dentry finish
[syscall ret --- Err] Getdents64 ret = Invalid argument
```

这说明 ext4 目录项读取已经成功完成，错误不是发生在 `read_dentry()` 内部，而是在 `sys_getdents64()` 后续处理阶段。

## 分析

`getdents01.c` 的 `run()` 流程很直接：

1. `SAFE_OPEN(".", O_RDONLY | O_DIRECTORY)` 打开当前临时目录。
2. `tst_getdents(fd, dirp, 512)` 调用当前 variant 对应的 `getdents/getdents64`。
3. 若返回值小于 0 且 errno 不是 `ENOSYS`，立即 `TFAIL`。
4. 按每条记录的 `d_reclen` 遍历返回缓冲区，检查是否完整包含 `.`, `..`, `dir`, `file`, `symlink`。

`sys_getdents64()` 原流程为：

```text
file.lseek(0, SEEK_CUR)
file.inode.read_dentry(off, len)
copy_to_user(...)
file.lseek(off, SEEK_SET)
```

日志已经证明 `read_dentry()` 返回成功，且 `copy_to_user()` 之前生成了目录项缓冲区。因此继续检查最后一步普通 `lseek(SEEK_SET)`。

目录项里的 `d_off` 是目录流 cookie，用来表示下一次目录遍历的位置；它不是普通文件的字节偏移。当前 ext4 目录遍历返回的下一项 cookie 可能不满足普通文件 `OSFile::lseek()` 的 seek 约束，导致 `lseek(SEEK_SET)` 返回 `EINVAL`。`sys_getdents64()` 使用 `?` 传播该错误后，用户态看到整个 `getdents64()` 失败，即使目录项实际上已经读取成功。

## 根因

`sys_getdents64()` 把目录流 `d_off` 当成普通文件 byte offset，通过通用 `OSFile::lseek(SEEK_SET)` 保存回 fd offset。

这混淆了两种语义：

- 普通文件 `lseek` offset：按文件大小和 seek 类型校验的字节位置。
- 目录流 offset/cookie：由底层目录遍历实现返回的下一次读取位置，不应再套用普通文件 seek 规则。

因此 `read_dentry()` 成功后，末尾更新 offset 的 `lseek` 反而把 syscall 变成 `EINVAL`。

## 修复

- 为 `OSFile` 增加 `set_offset(offset)`，只更新 fd 内部 offset，不走普通文件 `lseek` 校验。
- `sys_getdents64()` 在 `read_dentry()` 和 `copy_to_user()` 成功后，使用 `file.set_offset(off as usize)` 保存下一次目录读取位置。
- `sys_getdents64()` 先检查目标 fd 是否为目录，非目录返回 `ENOTDIR`，避免目录读取路径落到普通文件。
- `Ext4Inode::read_dentry()` 在用户缓冲区连第一条目录项都放不下时返回 `EINVAL`，否则按可容纳的完整目录项返回。
- `lwext4_rust::Ext4File::read_dir_from()` 不再信任缓存的 `this_type` 判断目录，而是按路径调用 `ext4_inode_exist(..., EXT4_DE_DIR)`；同时检查 `ext4_dir_open()` 返回值，并修正 `d_reclen` 的 8 字节对齐计算。

涉及文件：

- `os/src/syscall/fs/ctl.rs`
- `os/src/fs/files/os_file.rs`
- `os/src/fs/ext4_lw/inode.rs`
- `crates/lwext4_rust/src/file.rs`

## 验证

已执行：

```text
make
```

结果：

- 当前默认 `TARGET_ARCH=loongarch64`，构建通过。

维护者提供的最新 `log.ans` / `ana.ans` 显示 `getdents01` 已无 failed：

```text
getdents.h:151: TINFO: Testing the SYS_getdents64 syscall
getdents01.c:92: TINFO: Found '.'
getdents01.c:92: TINFO: Found '..'
getdents01.c:92: TINFO: Found 'dir'
getdents01.c:92: TINFO: Found 'file'
getdents01.c:92: TINFO: Found 'symlink'
getdents01.c:126: TPASS: All entries found
Summary:
passed   2
failed   0
broken   0
```

当前验证未重新运行 `riscv64`。
