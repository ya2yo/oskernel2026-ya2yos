# chown04 chown errno 语义修复

## 背景

LTP `chown04` 验证 `chown(2)` 在错误路径和权限场景下的 errno，包括非特权用户、路径前缀无搜索权限、坏用户指针、超长路径、不存在文件、路径前缀非目录、符号链接循环和只读文件系统。

## 现象

新的 `log.ans` 中，musl/glibc 两轮 `chown04` 均有 3 项失败：

```text
chown04.c:77: TFAIL: chown() without full permissions of the path prefix expected EACCES: EPERM (1)
chown04.c:77: TFAIL: chown() when pathname is too long expected ENAMETOOLONG: ENOENT (2)
chown04.c:77: TFAIL: chown() when the named file resides on a read-only filesystem expected EROFS: EPERM (1)
```

其余 `EPERM/EFAULT/ENOENT/ENOTDIR/ELOOP` 场景已通过。

## 分析

`chown04` 的 setup 会创建：

- `testdir_1`，权限为 `0700`，随后切到 `nobody` 用户；
- `testdir_1/tfile_2`，用于验证路径前缀缺少搜索权限时应返回 `EACCES`；
- 长度超过 `PATH_MAX` 的单分量路径，用于验证 `ENAMETOOLONG`；
- 只读 tmpfs 挂载点 `mntpoint`，用于验证 `EROFS`。

原 `sys_fchownat()` 读取用户路径后，直接解析绝对路径并 `open()` 目标文件，最后调用 `chown_inode()`。`chown_inode()` 首先检查当前 effective uid，不是 root 就返回 `EPERM`。这导致两个路径级错误被权限错误覆盖：

- 父目录缺少 execute/search 权限时，没有在修改 owner 前检查父目录搜索权限；
- 目标位于只读挂载点时，没有在特权检查前返回 `EROFS`。

此外 `sys_fchownat()` 缺少 chown 路径的 `MAX_PATH_LEN` / `NAME_MAX` 检查，超长单分量路径继续走普通路径查找，最终返回 `ENOENT`。

## 根因

`chown` 系列实现把 inode owner 修改权限检查放得过早，缺少路径解析阶段的 Linux errno 优先级处理：

1. 用户路径长度和单个路径分量长度未校验；
2. 普通路径目标打开前没有检查父目录搜索权限；
3. `chown_inode()` 没有根据目标路径检查只读挂载点，非 root 场景直接返回 `EPERM`。

## 修复

涉及文件：

- `os/src/syscall/fs/ctl.rs`

主要改动：

- 为 `fchownat()` 增加 `MAX_PATH_LEN` 和 `NAME_MAX=255` 路径分量检查，超长路径返回 `ENAMETOOLONG`；
- 新增 `check_parent_search_permission()`，普通路径目标打开前检查父目录类型和当前 effective uid/gid 的 execute/search 权限，缺少权限返回 `EACCES`；
- `chown_inode()` 增加可选路径参数，在 root/owner 修改权限检查前按 `MNT_TABLE.mount_for_path()` 检查只读挂载点，返回 `EROFS`；
- `fchownat()`、`fchown()` 和 `/proc/self/fd/<fd>` 兼容路径都向 `chown_inode()` 传递可解析的 inode 路径，保持 fd 路径也能执行只读挂载检查。

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml
make
/bin/bash -lc 'timeout 120s make run > /tmp/chown04-after-fix.log 2>&1'
```

当前默认架构为 LoongArch64，`make` 通过。

`/tmp/chown04-after-fix.log` 中 musl `chown04`：

```text
chown04.c:77: TPASS: chown() without permissions : EPERM (1)
chown04.c:77: TPASS: chown() without full permissions of the path prefix : EACCES (13)
chown04.c:77: TPASS: chown() with unaccessible pathname points : EFAULT (14)
chown04.c:77: TPASS: chown() when pathname is too long : ENAMETOOLONG (36)
chown04.c:77: TPASS: chown() when file does not exist : ENOENT (2)
chown04.c:77: TPASS: chown() when the path prefix is not a directory : ENOTDIR (20)
chown04.c:77: TPASS: chown() with too many symbolic links : ELOOP (40)
chown04.c:77: TPASS: chown() when the named file resides on a read-only filesystem : EROFS (30)
Summary:
passed   8
failed   0
broken   0
skipped  0
warnings 0
```

glibc `chown04` 同样 8 项 `TPASS`，summary 为：

```text
passed   8
failed   0
broken   0
skipped  0
warnings 0
```

日志中的 `tst_rmdir` `ELOOP` 是 cleanup 阶段清理测试符号链接环的 warning；核心测试 summary 已通过。包装行 `FAIL LTP CASE chown04 : 10` / `RESULT GLIBC LTP SINGLE CASE chown04 : 10` 不作为失败依据。
