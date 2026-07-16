# LTP open14 procfd linkat 校验顺序与 fstat panic

## 背景

LTP `open14` 通过 `open(".", O_TMPFILE | O_RDWR, ...)` 创建匿名文件，写入后以
`linkat(AT_FDCWD, "/proc/self/fd/<fd>", AT_FDCWD, "tmpfile",
AT_SYMLINK_FOLLOW)` 将其物化到当前目录。该用例分别在 musl 和 glibc 下运行。

## 现象

初始 `log.ans` 中，musl 与 glibc 都在上述 `linkat()` 返回 `ENOENT`，LTP 将其报告为
`TBROK`。修复 procfd 分支可达性后，glibc 继续在
`crates/lwext4_rust/src/file.rs` 的 `Ext4File::fstat()` 触发
`attempt to divide by zero` panic。

## 分析

`sys_linkat()` 先将旧路径 `/proc/self/fd/<fd>` 作为普通路径执行
`check_parent_permission(parent_path_of(old_abs_path))`。Ya2yOS 的 proc 实现只在
syscall 路径中提供该 magic-link，不会在 rootfs 中创建真实的 `/proc/self/fd` 目录，
因此权限检查中的 `open("/proc/self/fd")` 返回 `ENOENT`，后续已有的 O_TMPFILE
物化分支永远不会执行。

glibc 路径随后暴露了独立的错误处理问题：`ext4_stat_get()` 返回错误时，`fstat()`
仍先用默认全零 `ext4_inode_stat` 的 `st_blksize` 更新小文件缓存块数，之后才检查
返回码，导致整数除零。

消除 panic 后，glibc 深层目录阶段仍反复输出 `ext4_stat_get` 与
`write_back_cache ext4_fopen` 的 `ENOENT`。物化目标在 lwext4 小文件缓存中首次写入，
缓存淘汰时按 `O_RDWR` 重新打开该路径会失败。直接以创建模式重建又一度让 LTP 读到
mode 0，说明缓存还必须保存创建阶段设置的权限位。

## 根因

1. procfd magic-link 的识别顺序错误，将虚拟路径当作必须存在的真实父目录验证。
2. `Ext4File::fstat()` 的错误码检查晚于依赖返回结构体字段的缓存计算。
3. 小文件写回缓存没有表达“已创建、待物化”的状态，也没有保留待物化文件的 mode。

## 修复

- `sys_linkat()` 在通用源路径挂载与父目录检查前识别 `/proc/self/fd/<fd>`。该分支
  要求 `AT_SYMLINK_FOLLOW`，对目标目录继续执行只读挂载和写权限检查，然后按既有
  O_TMPFILE 兼容路径复制内容并登记 inode/dentry cache。
- `Ext4File::fstat()` 在释放 C 路径字符串后立即检查 `ext4_stat_get()` 返回值；失败
  时若命中小文件缓存，则返回缓存长度和保存的 regular-file mode；否则向调用者返回
  错误，不再访问未初始化元数据。
- `VFileCache` 保存待物化文件的 mode。`file_mode_set()` 将 mode 传给缓存；写回若
  首次 `O_RDWR` 打开返回 `ENOENT`，则以创建模式重建文件、恢复 mode 后写入缓存数据。
  `unlink` 已先移除对应缓存，故该补偿不会复活已删除文件。

## 涉及文件

- `os/src/syscall/fs/ctl.rs`
- `crates/lwext4_rust/src/file.rs`

## 验证

- `cargo fmt --manifest-path os/Cargo.toml -- --check` 通过。
- 分别执行 `make build-arch TARGET_ARCH=riscv64` 与
  `make build-arch TARGET_ARCH=loongarch64`，两份 release 构建均通过，仅有既有 vendored
  `smoltcp` warning。
- `timeout 300s make TARGET_ARCH=riscv64 run > log.ans 2>&1` 在允许 QEMU 使用
  `/var/tmp` 的环境中完成。musl 与 glibc `open14` 都为 `passed 3 failed 0 broken 0`
  并正常 `shutdown!`；日志无 `ERROR`、`panic`、`TFAIL` 或 `TBROK`。
