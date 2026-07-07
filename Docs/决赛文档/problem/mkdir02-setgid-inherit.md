# mkdir02 目录 S_ISGID 继承语义修复

## 背景

LTP `mkdir02` 验证 Linux `mkdir(2)` 在父目录设置 `S_ISGID` 时的继承语义：新建子目录需要继承父目录的 group ID，并保留 `S_ISGID` mode 位。

测试 setup 中会创建 `testdir1`，执行 `chmod(0777 | S_ISGID)` 和 `chown(getuid(), free_gid)`，随后切换到 nobody 用户，再在该父目录下创建 `testdir2` 并检查 `stat()` 结果。

## 现象

新的 `log.ans` 中，musl 与 glibc 两轮 `mkdir02` 都失败：

```text
mkdir02.c:41: TFAIL: New dir FAILED to inherit S_ISGID
Summary:
passed   0
failed   1
broken   0
skipped  0
warnings 0
```

日志没有报告 gid 继承失败，说明当前路径已经把新 inode 的 gid 设置为父目录 gid，但新目录自身 mode 缺少 `02000`。

## 分析

`sys_mkdirat()` 通过 `open(O_RDWR | O_CREATE | O_EXCL | O_DIRECTORY)` 复用统一创建路径，最终进入 `os/src/fs/kernel_fs_ops/open.rs` 的 `create_file()`。

该函数在创建 inode 后会应用进程 `umask` 得到 `effective_mode`，然后调用 `inode.fmode_set(effective_mode)`。后续 owner 设置里已经有父目录 `S_ISGID` 时继承父目录 gid 的逻辑：

```text
parent_mode & 0o2000 != 0 -> gid = parent_stat.st_gid
```

但 mode 设置阶段没有区分目录，也没有在父目录带 `S_ISGID` 时给新目录补 `02000`，因此 `statbuf.st_gid` 正确而 `statbuf.st_mode & S_ISGID` 为 0。

## 根因

目录创建公共路径只实现了父目录 `S_ISGID` 下的 gid 继承，遗漏了 Linux 对新建子目录继续继承 `S_ISGID` mode 位的语义。

同时，原代码忽略了 `inode.fmode_set()` 的返回值；如果底层 mode 写入失败，创建路径仍会继续返回成功，不利于暴露元数据写入错误。

## 修复

修改 `os/src/fs/kernel_fs_ops/open.rs`：

- 在创建前保存 `flags.node_type()`，避免重复解析 flags。
- `effective_mode = mode & !umask` 后，如果新建对象是目录且父目录 mode 带 `0o2000`，则为新目录补上 `0o2000`。
- 保持普通文件行为不变：普通文件仍按现有逻辑继承 gid，但不继承 `S_ISGID` mode 位。
- 将 `inode.fmode_set(effective_mode)` 改为 `inode.fmode_set(effective_mode)?`，让底层错误返回用户态。

## 涉及文件

- `os/src/fs/kernel_fs_ops/open.rs`

## 验证

已执行：

```text
make
make run
```

结果：

- 默认 LoongArch64 `make` 通过，构建输出仅包含既有 `smoltcp` vendor warning。
- LoongArch64 `make run` 中 musl `mkdir02` 输出 `mkdir02.c:46: TPASS: New dir inherited GID and S_ISGID`，Summary 为 `passed 1 failed 0 broken 0`。
- LoongArch64 `make run` 中 glibc `mkdir02` 同样 `TPASS`，Summary 为 `passed 1 failed 0 broken 0`。
