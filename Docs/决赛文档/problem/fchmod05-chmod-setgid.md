# fchmod05 chmod S_ISGID 清除语义

## 背景

LTP `fchmod05` 验证 Linux `fchmod(2)` 对目录 `S_ISGID` 位的特殊语义：

- 调用者是目录 owner，但不是 root；
- 调用者的 effective gid 或 supplementary groups 不匹配目录 gid；
- `fchmod(fd, 043777)` 应返回成功；
- 但最终目录 mode 应静默清除 `S_ISGID`，即保留目录类型位和其他权限位，去掉 `02000`。

## 现象

新的 `log.ans` 中 musl 和 glibc 两轮均失败：

```text
fchmod05.c:57: TINFO: Found unused GID 3: SUCCESS (0)
fchmod05.c:43: TFAIL: testdir: Incorrect modes 041777, Expected 043777

Summary:
passed   0
failed   1
broken   0
```

`fchmod05` 的失败打印会对实际 `st_mode` 做 `dir_mode & ~S_ISGID` 后显示；因此这里实际含义是内核保留了调用者请求的 `S_ISGID`，没有按 Linux 语义清除。

## 分析

原 `sys_fchmod()` 和 `sys_fchmodat()` 直接调用 `inode.fmode_set(mode)`：

- 未检查当前 effective uid 是否为 root 或目标 inode owner；
- 未检查目标路径所在挂载点是否只读；
- 未按调用者 gid 与目标 inode gid 关系处理 `S_ISGID`；
- 部分路径还忽略了 `fmode_set()` 的错误返回值。

这会让非 root owner 在不属于目标目录 gid 时仍能设置 `S_ISGID`，与 `fchmod05` 期望不符。

## 根因

chmod 类 syscall 缺少 Linux chmod 公共语义层，直接把用户传入的 mode 写入文件系统，导致 `S_ISGID` 清除规则没有执行。

## 修复

在 `os/src/syscall/fs/ctl.rs` 中新增 `chmod_inode()` helper，并让以下入口统一调用它：

- `sys_fchmod()`
- `sys_fchmodat(..., AT_EMPTY_PATH)`
- `/proc/self/fd/<fd>` 兼容路径
- 普通 path-based `fchmodat()`

`chmod_inode()` 当前执行：

- 如果已知 path 位于只读挂载点，返回 `EROFS`；
- 非 root 且非 inode owner 时返回 `EPERM`；
- 非 root 且 effective gid 不等于 inode gid 时，清除请求 mode 中的 `S_ISGID`；
- 传播 `inode.fmode_set()` 的错误返回值。

当前内核还没有保存完整 supplementary groups，`setgroups()` 也只是最小兼容；本用例目标目录 gid 是 `tst_get_free_gid()` 返回的未使用 gid，和 effective gid 不同，因此用 effective gid 判断即可覆盖当前可观察语义。

## 涉及文件

- `os/src/syscall/fs/ctl.rs`

## 验证

已执行：

```text
make
```

结果：默认 LoongArch64 构建通过。

尝试在沙箱内运行：

```text
timeout 120s make run > /tmp/fchmod05-chmod-fix.log 2>&1
```

但 QEMU 因当前沙箱 `/var/tmp` 只读限制未启动：

```text
Could not open temporary file '/var/tmp/...': Read-only file system
```

随后用户确认运行输出已写入最新 `log.ans`。复查 `log.ans` 后，musl 和 glibc 两轮 `fchmod05` 均通过：

```text
fchmod05.c:46: TPASS: Functionality of fchmod(3, 043777) successful

Summary:
passed   1
failed   0
broken   0
```

日志中的 `FAIL LTP CASE fchmod05 : 10` / `RESULT GLIBC LTP SINGLE CASE fchmod05 : 10` 是 wrapper 退出码打印；本次按 `TPASS` 与 Summary 判断，核心断言已通过。
