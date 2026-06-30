# open07: O_NOFOLLOW 与符号链接路径解析

## 背景

LTP `open07` 覆盖 `open(O_NOFOLLOW)` 对符号链接的语义：

- 最终路径分量是 symlink 时应返回 `ELOOP`。
- 路径中间分量是 symlink 目录时仍应跟随，`symdir1/testfile` 应能正常打开。
- setup 阶段会通过 `creat("symdir1/testfile", 0644)` 在 symlink 目录下创建文件。

## 现象

最初日志中 `open07` setup 失败：

```text
open07.c:51: TBROK: creat(symdir1/testfile,0644) failed: ENOENT (2)
```

修复创建路径后，继续暴露 `O_NOFOLLOW` 语义问题：最终分量为 symlink 的 `symfile1`、`symfile2`、`symdir1`、`symdir2` 曾被错误打开成功，而测试期望 `ELOOP`。

## 分析

`open07.c` 的 setup 先创建：

- `testdir`
- `symdir1 -> testdir`
- `symdir2 -> symdir1`
- `symdir1/testfile`

Ya2yOS 原 `create_file()` 直接把 `/tmp/.../symdir1/testfile` 交给底层 ext4 创建。lwext4 的创建接口不会在父目录创建阶段替 VFS 解析中间 symlink，因此找不到真实父目录，返回 `ENOENT`。

之后检查 `Ext4Inode::find()` 发现：

- 查找已有路径时只在完整路径命中 symlink 分支后递归解析。
- `O_NOFOLLOW` 没有阻止最终分量 symlink 被跟随。
- lwext4 的 `ext4_inode_exist(path, EXT4_DE_DIR)` 会把 symlink-to-dir 按目录打开，导致 `open("symdir1", O_NOFOLLOW)` 在进入 symlink 分支前就成功。
- `FsIndex` 可能缓存已经解析过的 symlink 目录路径，`O_NOFOLLOW` 若直接命中缓存会绕过真实路径检查。

## 根因

根因有三点：

1. 创建新文件时没有先把父目录解析到真实目录，导致中间分量 symlink 不能作为父目录使用。
2. `open(O_NOFOLLOW)` 缺少最终分量 symlink 的 `ELOOP` 语义。
3. `FsIndex` 缓存的是路径到 inode 的结果，可能保存“symlink 路径 -> 目标目录 inode”，不能在 `O_NOFOLLOW` 下直接信任。

## 修复

修改 `os/src/fs/kernel_fs_ops/open.rs`：

- 新增父子路径拆分和拼接辅助函数。
- 新增 `resolve_create_path()`，先解析父目录 inode，再用父目录真实 `path()` 拼回 basename。
- `create_file()` 使用解析后的真实路径执行创建、权限检查和缓存。
- 普通打开路径在原始查找失败后，尝试解析父目录并用真实路径重查，使 `symdir1/testfile` 这种中间 symlink 路径可打开。
- `O_NOFOLLOW` 打开时跳过 `FsIndex` 快路径，避免命中已解析 symlink 的缓存。

修改 `os/src/fs/ext4_lw/inode.rs`：

- `find()` 在 `O_NOFOLLOW` 且最终路径分量为 symlink 时返回 `ELOOP`。

修改 `crates/lwext4_rust/src/file.rs`：

- 新增 `Ext4File::is_symlink()`，通过 `ext4_readlink()` 判断路径最终分量本身是否是 symlink，覆盖 symlink-to-dir 被 `ext4_inode_exist(..., DIR)` 当目录打开的问题。
- 修复 `file_readlink()` 中 `CString::into_raw()` 后未释放的泄漏。

## 涉及文件

- `crates/lwext4_rust/src/file.rs`
- `os/src/fs/ext4_lw/inode.rs`
- `os/src/fs/kernel_fs_ops/open.rs`

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

结果：

- 当前默认架构为 `loongarch64`，`make` 构建通过。
- `open07` musl 轮次输出 5 项 `TPASS`，Summary 为 `passed 5 failed 0 broken 0`。
- `open07` glibc 轮次输出 5 项 `TPASS`，Summary 为 `passed 5 failed 0 broken 0`。
- 日志中的 `FAIL LTP CASE open07 : 10` / `RESULT GLIBC LTP SINGLE CASE open07 : 10` 是当前 initproc 包装层打印的退出码行；按项目规则以 LTP `TPASS` 和 `Summary` 为准。
- 未运行 `riscv64`。
