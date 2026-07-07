# linkat02 hard link 上限与 unlink symlink 语义修复

## 背景

LTP `linkat02` 是 `linkat(2)` 负向语义测试，覆盖 `ENAMETOOLONG`、`EEXIST`、`ELOOP`、`EACCES`、`EROFS` 和 `EMLINK`。其中 `EMLINK` 用例会先调用 `tst_fs_fill_hardlinks()`，不断为同一个普通文件创建 hard link，直到文件系统返回 `EMLINK`。

## 现象

当前内核运行 `linkat02\0` 时直接卡死。`log.ans` 只打印到：

```text
RUN LTP CASE linkat02
["linkat02\0"]
linkat02    0  TINFO  :  Found free device 0 '/dev/loop0'
QEMU: Terminated
```

测试没有进入任何 `TPASS/TFAIL` 断言。

## 分析

对照 `linkat02.c`，卡死点位于 `setup()` 中的：

```c
max_hardlinks = tst_fs_fill_hardlinks(cleanup, "emlink_dir");
```

LTP 的 `tst_fs_fill_hardlinks()` 会创建 `emlink_dir/testfile0`，再循环调用 `link()` 创建 `testfile1..testfile65534`。只有遇到 `EMLINK` 时才返回非零 hard link 上限；如果一直成功，则要创建 65535 个目录项，在 QEMU 中表现为长时间无输出。

当前 `sys_linkat()` 在普通 hard link 分支只检查路径、父目录权限、挂载点和目标是否存在，之后直接调用底层 `hard_link()`，没有根据源 inode 的 `st_nlink` 返回 `EMLINK`。

修复 `EMLINK` 后，7 个断言都能 `TPASS`，但 cleanup 阶段仍出现：

```text
TWARN: tst_rmdir: rmobj(...) failed: unlink(.../testeloop) failed; errno=40: ELOOP
```

原因是 `sys_unlinkat()` 通过普通 `open()` 打开目标，最终 symlink 会被跟随；`linkat02` 创建了 `testeloop -> test_file_eloop2 -> testeloop` 的 symlink 环，cleanup 的 `unlink("testeloop")` 应删除 symlink 本身，而不是解析到环并返回 `ELOOP`。

## 根因

1. `linkat(2)` 缺少 hard link 数量上限检查，导致 LTP 的 hard link 上限探测循环无法及时收敛。
2. `unlinkat(2)` 错误跟随最终 symlink，导致 cleanup 删除 symlink 环时返回 `ELOOP`，产生 warning 和非零退出码。

## 修复

在 `os/src/syscall/fs/ctl.rs` 中：

- 新增 `MAX_HARD_LINKS = 1024`。POSIX 只要求 `LINK_MAX >= 8`，该上限足够兼容测试，并能让 hard link 探测快速结束。
- 新增 `check_hard_link_limit()`，在 `AT_EMPTY_PATH` hard link 分支和普通 hard link 分支调用；当源 inode `st_nlink >= MAX_HARD_LINKS` 时返回 `EMLINK`。
- `sys_unlinkat()` 打开目标时加入 `OpenFlags::O_UNLINK`，让普通 unlink 删除最终 symlink 自身，不跟随 symlink 环。

## 涉及文件

- `os/src/syscall/fs/ctl.rs`

## 验证

已执行：

```text
make
timeout 180s make run > /tmp/linkat02-unlink-symlink.log 2>&1
```

结果：

- 默认 LoongArch64 `make` 通过，只有既有 `smoltcp` vendor warning。
- LoongArch64 musl `linkat02`：7 项 `TPASS`，summary 为 `passed 7 failed 0 broken 0 warnings 0`。
- LoongArch64 glibc `linkat02`：7 项 `TPASS`，summary 为 `passed 7 failed 0 broken 0 warnings 0`。
- wrapper 行为：`FAIL LTP CASE linkat02 : 0` 和 `RESULT GLIBC LTP SINGLE CASE linkat02 : 0`，即测试退出码为 0。
