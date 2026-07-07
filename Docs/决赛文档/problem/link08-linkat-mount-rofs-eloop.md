# link08 linkat mount/rofs/ELOOP 语义修复

## 背景

LTP `link08` 覆盖 `link(2)` 的 4 类负向语义：旧路径是目录时返回 `EPERM`，跨挂载点创建 hard link 返回 `EXDEV`，只读文件系统内创建 hard link 返回 `EROFS`，解析路径遇到过多符号链接返回 `ELOOP`。

## 现象

新的 `log.ans` 中，musl 和 glibc 的 `link08` 均只有目录旧路径用例通过，其余 3 项失败：

- `link("mntpoint/file", "testfile")` 错误成功，期望 `EXDEV`。
- `link("mntpoint/file", "mntpoint/testfile4")` 错误成功，期望 `EROFS`。
- `link("./test_eloop/...", "testfile")` 返回 `ENAMETOOLONG`，期望 `ELOOP`。

## 分析

`link08` 使用 LTP 的只读挂载前置条件，运行时会把 tmpfs 以只读标志挂载到 `mntpoint`。当前 `sys_linkat()` 普通分支只检查路径存在性、源 inode 是否为目录、新路径是否已存在和父目录权限，没有比较旧路径和新路径所在挂载点，因此允许跨挂载点 hard link；也没有在新路径所在挂载点为只读时提前返回 `EROFS`。

符号链接环用例构造了 `test_eloop/test_eloop -> ../test_eloop`，再拼接多层 `/test_eloop`。由于内核 `read_user_cstr()` 最多读取 `MAX_PATH_LEN` 字节，之前为修复 `link04` 加的旧路径长度预检在真正解析路径前直接返回 `ENAMETOOLONG`，遮蔽了中间 symlink loop 应返回的 `ELOOP`。

## 根因

`sys_linkat()` 缺少 hard link 的 mount 边界与只读挂载检查；同时旧路径过长的 errno 优先级处理过早，未给符号链接环解析留下机会。

## 修复

在 `os/src/syscall/fs/ctl.rs` 中补充：

- `check_link_mounts()`：查询旧路径和新路径覆盖的最深挂载点；挂载点不同返回 `EXDEV`，同一只读挂载点返回 `EROFS`。
- `check_link_path(..., check_length)`：旧路径入口只做空路径检查，长度检查延后到解析绝对路径之后；新路径仍保持入口长度检查。
- `has_self_referential_symlink_prefix()`：当旧路径达到内核读取上限时，使用 `O_UNLINK` 打开路径前缀中的 symlink inode，读取相对目标并规范化；若目标回到当前 symlink 前缀或其祖先，优先返回 `ELOOP`。

这样 `link04` 的普通超长路径仍返回 `ENAMETOOLONG`，而 `link08` 的 symlink loop 返回 `ELOOP`。

## 涉及文件

- `os/src/syscall/fs/ctl.rs`

## 验证

已执行：

```text
make
timeout 120s make run > /tmp/link08-after-eloop2.log 2>&1
```

结果：

- 默认 LoongArch64 `make` 通过，只有既有 `smoltcp` vendor warning。
- LoongArch64 musl `link08`：4 项 `TPASS`，summary 为 `passed 4 failed 0 broken 0`。
- LoongArch64 glibc `link08`：4 项 `TPASS`，summary 为 `passed 4 failed 0 broken 0`。
